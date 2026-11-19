// SPDX-License-Identifier: CC0-1.0

//! WHERE this service publishes its data plane, and that the address it publishes is one
//! this emulator answers on.
//!
//! `IngestEndpoints[].Url` on a channel and the `Url` of every manifest an origin endpoint
//! publishes exist for a client to DIAL later. AWS serves both on per-group hostnames under
//! its own regional domain, which a client on the machine this emulator runs on resolves
//! nowhere at all — so the rule is: the address a group's data plane is announced at is the
//! endpoint the request that CREATED the group reached, with everything that hostname
//! carried moved into the path, and the published URL is answered by this service with an
//! AUTHORIZATION decision rather than by a routing failure.
//!
//! Both sides of the rule are asserted over a table that spans the addressings a caller can
//! reach the emulator by — an IPv4 loopback address, a loopback name, a bracketed IPv6
//! literal, each with a port — plus the caller that addressed one of AWS's own hostnames,
//! which is announced the hostname this service publishes there. The resource names are not
//! what the rule generalises over; the ADDRESS is.
//!
//! The manifest half of the rule is asserted along the other axes it generalises over as
//! well, since an origin endpoint publishes a URL per manifest rather than one per resource:
//! the manifest FAMILY (and so the container type and the URL suffix), how MANY manifests
//! one endpoint publishes, how many endpoints a channel holds, and which of the four
//! origin-endpoint operations re-reports the URL — the create, the read, the update and the
//! listing, whose item shape wraps the same members one level deeper.

use serde_json::{json, Value};

// The manifest families are the ONE catalog of them in this crate's suites, owned (and
// sourced from the model) by `published_data_plane_urls`, so neither suite can drift from
// the model's container/family pairings on its own.
use crate::published_data_plane_urls::{manifest_url, EgressRow, EGRESS_ROWS};
use crate::rejection_rule_and_negative_paths::{encoded, grant_policy, split_url, Answer, Emulator};

/// The Region every request in this suite is signed for: it travels in the SigV4 credential
/// scope, which is what the service reads its Region from, and is the Region whose
/// resources the published address serves.
const REGION: &str = "us-west-2";

/// The names the fixtures are created under.
const GROUP: &str = "addr-grp";
const CHANNEL: &str = "addr-chan";
const ENDPOINT: &str = "addr-ep";
const MANIFEST: &str = "index";

/// The reserved path segment every data plane this emulator serves on its own endpoint sits
/// under, and the segment that names THIS service's plane behind it.
const LOCAL_PREFIX: &str = "/_aws/mediapackagev2";

/// One row: the authority the creating request addressed, and whether that authority is one
/// of AWS's own hostnames (which is a caller that resolves them) or an address of this
/// emulator (which is a caller that resolves nothing else).
#[derive(Debug, Clone, Copy)]
struct Addressing {
    label: &'static str,
    authority: &'static str,
    emulators_own: bool,
}

/// The rows: every shape the emulator's own endpoint takes, and one of AWS's own hostnames.
const ADDRESSINGS: &[Addressing] = &[
    Addressing {
        label: "an IPv4 loopback address with a port",
        authority: "127.0.0.1:9297",
        emulators_own: true,
    },
    Addressing {
        label: "a loopback name with a port",
        authority: "localhost:39355",
        emulators_own: true,
    },
    Addressing {
        label: "a bracketed IPv6 literal with a port",
        authority: "[::1]:9297",
        emulators_own: true,
    },
    Addressing {
        label: "one of AWS's own hostnames",
        authority: "mediapackagev2.us-west-2.amazonaws.com",
        emulators_own: false,
    },
];

/// Send one signed control-plane request to `authority`: the credential scope is what the
/// service reads its Region from, so every row carries one whatever host it addressed.
async fn control(
    emulator: &Emulator,
    authority: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    emulator.send(method, path, body, authority, true, &[]).await
}

/// The first `Url` of a published list member (`IngestEndpoints`, `HlsManifests`), read
/// through the crate's existing extractor of that member shape.
fn published_url(answer: &Answer, member: &str) -> String {
    manifest_url(&answer.json, member)
}

/// Build the group, channel and origin endpoint for one row, and report what the creates
/// published: the group's `EgressDomain`, the channel's first ingest URL and the endpoint's
/// first HLS manifest URL.
async fn fixtures(emulator: &Emulator, row: &Addressing) -> (String, String, String) {
    let label = row.label;
    let group = control(
        emulator,
        row.authority,
        "POST",
        "/channelGroup",
        Some(json!({"ChannelGroupName": GROUP})),
    )
    .await;
    assert_eq!(
        group.status, 200,
        "case {label:?}: CreateChannelGroup expected 200, got {} with body {}",
        group.status, group.body
    );
    let channel = control(
        emulator,
        row.authority,
        "POST",
        &format!("/channelGroup/{GROUP}/channel"),
        Some(json!({"ChannelName": CHANNEL, "InputType": "CMAF"})),
    )
    .await;
    assert_eq!(
        channel.status, 200,
        "case {label:?}: CreateChannel expected 200, got {} with body {}",
        channel.status, channel.body
    );
    let endpoint = control(
        emulator,
        row.authority,
        "POST",
        &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint"),
        Some(json!({
            "OriginEndpointName": ENDPOINT,
            "ContainerType": "TS",
            "HlsManifests": [{"ManifestName": MANIFEST}],
        })),
    )
    .await;
    assert_eq!(
        endpoint.status, 200,
        "case {label:?}: CreateOriginEndpoint expected 200, got {} with body {}",
        endpoint.status, endpoint.body
    );
    (
        group.string("EgressDomain").to_string(),
        published_url(&channel, "IngestEndpoints"),
        published_url(&endpoint, "HlsManifests"),
    )
}

#[tokio::test]
async fn a_group_publishes_its_data_plane_at_the_address_the_create_reached() {
    for row in ADDRESSINGS {
        let label = row.label;
        let emulator = Emulator::new();
        let (egress_domain, ingest_url, manifest_url) = fixtures(&emulator, row).await;

        if row.emulators_own {
            assert_eq!(
                egress_domain, row.authority,
                "case {label:?}: a caller that reached the emulator's own endpoint must be \
                 given that endpoint, got {egress_domain:?}"
            );
            // Everything the published hostname carried moves into the path: the plane's
            // own namespace, this service's segment, and the Region the address serves.
            let prefix = format!("https://{}{LOCAL_PREFIX}/{REGION}", row.authority);
            for (member, url, aws_path) in [
                ("IngestEndpoints", &ingest_url, "/in/v1/"),
                ("HlsManifests", &manifest_url, "/out/v1/"),
            ] {
                assert!(
                    url.starts_with(&prefix),
                    "case {label:?}: the published {member} URL must be on that endpoint \
                     under {prefix:?}, got {url:?}"
                );
                assert!(
                    url.contains(aws_path),
                    "case {label:?}: the published {member} URL must keep the service's own \
                     data-plane path {aws_path:?}, got {url:?}"
                );
                assert!(
                    !url.contains("amazonaws.com"),
                    "case {label:?}: the published {member} URL must name no host this \
                     emulator does not serve, got {url:?}"
                );
            }
        } else {
            assert!(
                egress_domain.contains(".egress.") && egress_domain.ends_with(".amazonaws.com"),
                "case {label:?}: a caller that resolves AWS's own hosts must be given the \
                 hostname this service publishes there, got {egress_domain:?}"
            );
            assert!(
                manifest_url.starts_with(&format!("https://{egress_domain}/out/v1/")),
                "case {label:?}: the manifest URL must be the group's egress domain plus \
                 the service's own path, got {manifest_url:?}"
            );
            assert!(
                ingest_url.contains(".ingest.") && ingest_url.contains("/in/v1/"),
                "case {label:?}: the ingest URL must be published on the group's ingest \
                 DNS, got {ingest_url:?}"
            );
            assert!(
                !ingest_url.contains(LOCAL_PREFIX),
                "case {label:?}: a published hostname carries the Region itself, so the \
                 path must not, got {ingest_url:?}"
            );
        }

        // Whatever the addressing, every reader reports the address the create chose.
        let read = control(
            &emulator,
            row.authority,
            "GET",
            &format!("/channelGroup/{GROUP}"),
            None,
        )
        .await;
        assert_eq!(
            read.string("EgressDomain"),
            egress_domain,
            "case {label:?}: GetChannelGroup must report the announced domain, got {}",
            read.body
        );
    }
}

#[tokio::test]
async fn the_published_address_is_answered_by_this_services_data_plane() {
    for row in ADDRESSINGS {
        let label = row.label;
        let emulator = Emulator::new();
        let (_egress_domain, ingest_url, manifest_url) = fixtures(&emulator, row).await;

        // EGRESS: before any grant the fetch is not authorized, which is this service
        // answering rather than the front door failing to route.
        let (host, path) = split_url(&manifest_url);
        let denied = emulator.send("GET", &path, None, &host, false, &[]).await;
        assert_eq!(
            denied.status, 403,
            "case {label:?}: a fetch with no read grant expected HTTP 403, got {} with body \
             {}",
            denied.status, denied.body
        );
        assert!(
            denied.body.to_lowercase().contains("access denied"),
            "case {label:?}: the refusal must say access denied, got {}",
            denied.body
        );

        // With the grant in place the same URL is authorized and reaches the object
        // lookup, which is the OTHER state a player branches on.
        let granted = control(
            &emulator,
            row.authority,
            "POST",
            &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}/policy"),
            Some(json!({"Policy": grant_policy("mediapackagev2:GetObject")})),
        )
        .await;
        assert_eq!(
            granted.status, 200,
            "case {label:?}: PutOriginEndpointPolicy expected 200, got {} with body {}",
            granted.status, granted.body
        );
        let authorized = emulator.send("GET", &path, None, &host, false, &[]).await;
        assert_eq!(
            authorized.status, 404,
            "case {label:?}: an authorized fetch with nothing packaged expected HTTP 404, \
             got {} with body {}",
            authorized.status, authorized.body
        );

        // INGEST: MediaPackage vends no ingest credential, so a push presenting none is
        // refused by this service; one the emulator authenticated is recorded.
        let (ingest_host, ingest_path) = split_url(&ingest_url);
        let object = format!("{ingest_path}/0.mp4");
        let unauthenticated = emulator
            .send_raw("PUT", &object, "segment".to_string(), &ingest_host, false, &[])
            .await;
        assert_eq!(
            unauthenticated.status, 403,
            "case {label:?}: an unauthenticated push expected HTTP 403, got {} with body {}",
            unauthenticated.status, unauthenticated.body
        );
        assert!(
            unauthenticated.body.to_lowercase().contains("access denied"),
            "case {label:?}: the refusal must say access denied, got {}",
            unauthenticated.body
        );
        let pushed = emulator
            .send_raw("PUT", &object, "segment".to_string(), &ingest_host, true, &[])
            .await;
        assert_eq!(
            pushed.status, 200,
            "case {label:?}: a credentialed push expected HTTP 200, got {} with body {}",
            pushed.status, pushed.body
        );
    }
}

#[tokio::test]
async fn a_path_on_the_emulators_endpoint_naming_another_region_resolves_nothing() {
    // The Region a local address carries is in the PATH, so it is what decides which
    // Region's resources the request reaches — exactly as the published hostname's Region
    // label does. A path naming a Region whose store holds no such group is a fetch of
    // nothing, not of another Region's endpoint.
    let row = ADDRESSINGS[0];
    let emulator = Emulator::new();
    let (_egress_domain, _ingest_url, manifest_url) = fixtures(&emulator, &row).await;
    let (host, path) = split_url(&manifest_url);
    let granted = control(
        &emulator,
        row.authority,
        "POST",
        &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}/policy"),
        Some(json!({"Policy": grant_policy("mediapackagev2:GetObject")})),
    )
    .await;
    assert_eq!(
        granted.status, 200,
        "setup: PutOriginEndpointPolicy expected 200, got {} with body {}",
        granted.status, granted.body
    );

    // Each row is the same path with the Region segment replaced: the home Region reads
    // the granted endpoint, every other Region reads nothing and is refused.
    for other in ["us-east-1", "eu-west-3", "ap-southeast-2"] {
        let elsewhere = path.replace(&format!("/{REGION}/"), &format!("/{other}/"));
        assert_ne!(
            elsewhere, path,
            "case {other:?}: the Region segment must be part of the published path, got \
             {path:?}"
        );
        let answer = emulator.send("GET", &elsewhere, None, &host, false, &[]).await;
        assert_eq!(
            answer.status, 403,
            "case {other:?}: a path naming a Region holding no such endpoint expected HTTP \
             403, got {} with body {}",
            answer.status, answer.body
        );
    }
    let home = emulator.send("GET", &path, None, &host, false, &[]).await;
    assert_eq!(
        home.status, 404,
        "case {REGION:?}: the granted endpoint's own Region must be authorized, got {} with \
         body {}",
        home.status, home.body
    );
}

#[tokio::test]
async fn the_announced_address_survives_a_tag_round_trip_on_the_group() {
    // The address is resolved ONCE, when the group is created, and stored: a later request
    // that reaches the emulator by a different addressing reads the address the create
    // chose, never one computed for the reader.
    let emulator = Emulator::new();
    let row = ADDRESSINGS[0];
    let (egress_domain, _ingest_url, manifest_url) = fixtures(&emulator, &row).await;
    let arn = format!(
        "arn:aws:mediapackagev2:{REGION}:000000000000:channelGroup/{GROUP}"
    );
    let tagged = emulator
        .send(
            "POST",
            &format!("/tags/{}", encoded(&arn)),
            Some(json!({"tags": {"team": "video"}})),
            "mediapackagev2.us-west-2.amazonaws.com",
            true,
            &[],
        )
        .await;
    assert_eq!(
        tagged.status, 204,
        "TagResource expected 204, got {} with body {}",
        tagged.status, tagged.body
    );
    let read = control(
        &emulator,
        "mediapackagev2.us-west-2.amazonaws.com",
        "GET",
        &format!("/channelGroup/{GROUP}"),
        None,
    )
    .await;
    assert_eq!(
        read.string("EgressDomain"),
        egress_domain,
        "a reader on another addressing must report the address the create chose, got {}",
        read.body
    );
    let endpoint = control(
        &emulator,
        "mediapackagev2.us-west-2.amazonaws.com",
        "GET",
        &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}"),
        None,
    )
    .await;
    assert_eq!(
        published_url(&endpoint, "HlsManifests"),
        manifest_url,
        "the stored manifest URL must be the one the create published, got {}",
        endpoint.body
    );
}

// ---------------------------------------------------------------------------
// The manifest half of the rule, along the axes an origin endpoint adds to it: the
// manifest family, the number of manifests, the number of endpoints, and the operation
// re-reporting the URL.
// ---------------------------------------------------------------------------

/// The channel the family rows' endpoints hang under. Its input type is `CMAF`, which every
/// container type below is servable by.
const FAMILY_CHANNEL: &str = "addr-families";

/// A second channel holding exactly ONE endpoint, so the list-wrapped form is asserted both
/// with many items and with one.
const SOLO_CHANNEL: &str = "addr-solo";

/// How many manifests one endpoint publishes, per row: the single-manifest shape, the
/// multiple-manifest shape, and one beyond it — every count within
/// `MIN_MANIFESTS_PER_ORIGIN_ENDPOINT..=MAX_MANIFESTS_PER_ORIGIN_ENDPOINT` publishes a URL
/// of its own.
const MANIFEST_COUNTS: &[usize] = &[1, 2, 3];

/// The create/update body of an endpoint publishing `manifests` of one family.
fn endpoint_body(row: &EgressRow, manifests: &[String]) -> Value {
    let mut body = json!({"ContainerType": row.container});
    body[row.family] = Value::Array(
        manifests
            .iter()
            .map(|name| json!({"ManifestName": name}))
            .collect(),
    );
    body
}

/// Every `(ManifestName, Url)` pair a response — or one item of a listing — carries under
/// `member`, in the order it serialized them.
fn published_manifests(value: &Value, member: &str) -> Vec<(String, String)> {
    value
        .get(member)
        .and_then(Value::as_array)
        .map(|manifests| {
            manifests
                .iter()
                .map(|manifest| {
                    let text = |name: &str| {
                        manifest
                            .get(name)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    (text("ManifestName"), text("Url"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The URL one manifest must be announced at: this service's own egress path under the
/// address the group's data plane is announced on — the emulator's own endpoint with
/// everything the published hostname carried moved into the path, or, for a caller that
/// resolves AWS's own hosts, the hostname published there.
fn announced_manifest_url(
    row: &Addressing,
    egress_domain: &str,
    channel: &str,
    endpoint: &str,
    manifest: &str,
    suffix: &str,
) -> String {
    let path = format!("/out/v1/{GROUP}/{channel}/{endpoint}/{manifest}{suffix}");
    if row.emulators_own {
        format!("https://{}{LOCAL_PREFIX}/{REGION}{path}", row.authority)
    } else {
        format!("https://{egress_domain}{path}")
    }
}

/// What one endpoint announced, for the listing to be judged against: the endpoint's name,
/// the member its manifests travel in, and the `(ManifestName, Url)` pairs the create
/// published.
struct Announced {
    endpoint: String,
    member: &'static str,
    manifests: Vec<(String, String)>,
}

/// Create the group and the two channels the family rows hang under, and report the group's
/// published egress domain.
async fn family_fixtures(emulator: &Emulator, row: &Addressing) -> String {
    let label = row.label;
    let group = control(
        emulator,
        row.authority,
        "POST",
        "/channelGroup",
        Some(json!({"ChannelGroupName": GROUP})),
    )
    .await;
    assert_eq!(
        group.status, 200,
        "case {label:?}: CreateChannelGroup expected 200, got {} with body {}",
        group.status, group.body
    );
    for channel in [FAMILY_CHANNEL, SOLO_CHANNEL] {
        let created = control(
            emulator,
            row.authority,
            "POST",
            &format!("/channelGroup/{GROUP}/channel"),
            Some(json!({"ChannelName": channel, "InputType": "CMAF"})),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {label:?} / {channel}: CreateChannel expected 200, got {} with body {}",
            created.status, created.body
        );
    }
    group.string("EgressDomain").to_string()
}

#[tokio::test]
async fn every_manifest_url_is_announced_on_the_address_the_group_was_created_through() {
    for row in ADDRESSINGS {
        let emulator = Emulator::new();
        let egress_domain = family_fixtures(&emulator, row).await;
        // The address every announced URL must be rooted on: the emulator's own endpoint for
        // a caller that reached it there, and the published hostname for one that resolves
        // AWS's own hosts — which that caller is owed in the form this service publishes
        // there, asserted independently of the value the group reported so the row carries
        // an expectation of its own.
        let expected_host = if row.emulators_own {
            row.authority.to_string()
        } else {
            assert!(
                egress_domain.contains(".egress.") && egress_domain.ends_with(".amazonaws.com"),
                "case {:?}: a caller that resolves AWS's own hosts must be given the hostname \
                 this service publishes there, got {egress_domain:?}",
                row.label
            );
            egress_domain.clone()
        };
        // What the listing must re-report, per endpoint.
        let mut expected: Vec<Announced> = Vec::new();
        let mut announced: Vec<String> = Vec::new();

        for family in EGRESS_ROWS {
            for &count in MANIFEST_COUNTS {
                let endpoint = format!("{}-{count}", family.endpoint);
                let manifests: Vec<String> = (1..=count)
                    .map(|index| format!("{}-{count}-{index}", family.manifest))
                    .collect();
                let case = format!(
                    "{} / {} / {count} manifest(s)",
                    row.label, family.label
                );
                let body = endpoint_body(family, &manifests);
                let want: Vec<(String, String)> = manifests
                    .iter()
                    .map(|manifest| {
                        (
                            manifest.clone(),
                            announced_manifest_url(
                                row,
                                &egress_domain,
                                FAMILY_CHANNEL,
                                &endpoint,
                                manifest,
                                family.suffix,
                            ),
                        )
                    })
                    .collect();

                let created = control(
                    &emulator,
                    row.authority,
                    "POST",
                    &format!("/channelGroup/{GROUP}/channel/{FAMILY_CHANNEL}/originEndpoint"),
                    Some({
                        let mut create = body.clone();
                        create["OriginEndpointName"] = json!(endpoint);
                        create
                    }),
                )
                .await;
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: CreateOriginEndpoint expected 200, got {} with body {}",
                    created.status, created.body
                );
                let announced_by_create = published_manifests(&created.json, family.family);
                assert_eq!(
                    announced_by_create, want,
                    "case {case:?}: every manifest must be announced at this service's \
                     egress path on {expected_host:?}, got {announced_by_create:?}"
                );

                // Each URL is its own address and names the host the group's plane is
                // announced on — not a host in a zone AWS publishes its own endpoints in.
                for (manifest, url) in &want {
                    let (host, path) = split_url(url);
                    assert_eq!(
                        host, expected_host,
                        "case {case:?} / {manifest}: the announced host must be the one the \
                         group's plane is announced on, got {url:?}"
                    );
                    assert!(
                        path.contains(&format!("/out/v1/{GROUP}/{FAMILY_CHANNEL}/{endpoint}/")),
                        "case {case:?} / {manifest}: the announced path must name the group, \
                         the channel and the endpoint, got {url:?}"
                    );
                    assert!(
                        !announced.iter().any(|seen| seen == url),
                        "case {case:?} / {manifest}: every manifest's URL must be distinct, \
                         got {url:?} twice"
                    );
                    announced.push(url.clone());
                }

                // The read re-reports exactly what the create announced.
                let endpoint_path = format!(
                    "/channelGroup/{GROUP}/channel/{FAMILY_CHANNEL}/originEndpoint/{endpoint}"
                );
                let read =
                    control(&emulator, row.authority, "GET", &endpoint_path, None).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: GetOriginEndpoint expected 200, got {} with body {}",
                    read.status, read.body
                );
                let announced_by_read = published_manifests(&read.json, family.family);
                assert_eq!(
                    announced_by_read, want,
                    "case {case:?}: GetOriginEndpoint must re-report the announced URLs, got \
                     {announced_by_read:?}"
                );

                // So does an update that reconfigures the endpoint around the same
                // manifests: the announced address survives the modify path.
                let updated = control(
                    &emulator,
                    row.authority,
                    "PUT",
                    &endpoint_path,
                    Some({
                        let mut update = body.clone();
                        update["Description"] = json!("reconfigured");
                        update
                    }),
                )
                .await;
                assert_eq!(
                    updated.status, 200,
                    "case {case:?}: UpdateOriginEndpoint expected 200, got {} with body {}",
                    updated.status, updated.body
                );
                let announced_by_update = published_manifests(&updated.json, family.family);
                assert_eq!(
                    announced_by_update, want,
                    "case {case:?}: UpdateOriginEndpoint must re-report the announced URLs, \
                     got {announced_by_update:?}"
                );

                expected.push(Announced {
                    endpoint,
                    member: family.family,
                    manifests: want,
                });
            }
        }

        // The listing wraps the same members one level deeper, and every item keeps its own
        // endpoint's URLs. `maxResults` is raised to one page so the whole channel is
        // compared at once (`ListOriginEndpointsRequest$MaxResults`, range 1..=100).
        let listed = control(
            &emulator,
            row.authority,
            "GET",
            &format!(
                "/channelGroup/{GROUP}/channel/{FAMILY_CHANNEL}/originEndpoint?maxResults=100"
            ),
            None,
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: ListOriginEndpoints expected 200, got {} with body {}",
            row.label, listed.status, listed.body
        );
        let items = listed
            .json
            .get("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            items.len(),
            expected.len(),
            "case {:?}: the listing must report every endpoint of the channel on one page, \
             got {}",
            row.label,
            listed.body
        );
        for want in &expected {
            let case = format!("{} / listed {}", row.label, want.endpoint);
            let item = items
                .iter()
                .find(|item| {
                    item.get("OriginEndpointName").and_then(Value::as_str)
                        == Some(want.endpoint.as_str())
                })
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the listing must report it, got {}", listed.body)
                });
            let listed_manifests = published_manifests(item, want.member);
            assert_eq!(
                listed_manifests, want.manifests,
                "case {case:?}: a listed endpoint must keep its own announced URLs, got \
                 {listed_manifests:?}"
            );
        }

        // The single-item listing is the same rule with one endpoint in the channel, which
        // is how most clients meet it.
        let solo = "solo-ep";
        let created = control(
            &emulator,
            row.authority,
            "POST",
            &format!("/channelGroup/{GROUP}/channel/{SOLO_CHANNEL}/originEndpoint"),
            Some(json!({
                "OriginEndpointName": solo,
                "ContainerType": "TS",
                "HlsManifests": [{"ManifestName": "index"}],
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreateOriginEndpoint expected 200, got {} with body {}",
            row.label, created.status, created.body
        );
        let want = vec![(
            "index".to_string(),
            announced_manifest_url(
                row,
                &egress_domain,
                SOLO_CHANNEL,
                solo,
                "index",
                ".m3u8",
            ),
        )];
        let listed = control(
            &emulator,
            row.authority,
            "GET",
            &format!("/channelGroup/{GROUP}/channel/{SOLO_CHANNEL}/originEndpoint"),
            None,
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: ListOriginEndpoints expected 200, got {} with body {}",
            row.label, listed.status, listed.body
        );
        let items = listed
            .json
            .get("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            items.len(),
            1,
            "case {:?}: a channel holding one endpoint must list one item, got {}",
            row.label,
            listed.body
        );
        let listed_manifests = published_manifests(&items[0], "HlsManifests");
        assert_eq!(
            listed_manifests, want,
            "case {:?}: the single listed endpoint must keep its announced URL, got {}",
            row.label, listed.body
        );
    }
}

#[tokio::test]
async fn every_familys_announced_manifest_url_is_answered_by_this_service() {
    // The point of the rule: the announced address is one a player can fetch without
    // leaving the emulator. Each family's URL is dialled exactly as published — host and
    // path as they came back — and answered by this service's own authorization decision:
    // refused until the endpoint's policy grants the read, and the object lookup once it
    // does.
    //
    // Only the rows announced on the EMULATOR'S OWN endpoint are swept here: the hostname a
    // caller that resolves AWS's own hosts is given is dialled per family already, by
    // `published_data_plane_urls::every_published_manifest_url_is_gated_by_the_endpoints_resource_policy`.
    for row in ADDRESSINGS.iter().filter(|row| row.emulators_own) {
        let emulator = Emulator::new();
        let egress_domain = family_fixtures(&emulator, row).await;
        for family in EGRESS_ROWS {
            let case = format!("{} / {}", row.label, family.label);
            let endpoint = format!("{}-served", family.endpoint);
            let manifest = format!("{}-served", family.manifest);
            let mut body = endpoint_body(family, ::std::slice::from_ref(&manifest));
            body["OriginEndpointName"] = json!(endpoint);
            let created = control(
                &emulator,
                row.authority,
                "POST",
                &format!("/channelGroup/{GROUP}/channel/{FAMILY_CHANNEL}/originEndpoint"),
                Some(body),
            )
            .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreateOriginEndpoint expected 200, got {} with body {}",
                created.status, created.body
            );
            let published = published_manifests(&created.json, family.family);
            let url = published
                .first()
                .map(|(_name, url)| url.clone())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no URL was announced, got {}", created.body)
                });
            assert_eq!(
                url,
                announced_manifest_url(
                    row,
                    &egress_domain,
                    FAMILY_CHANNEL,
                    &endpoint,
                    &manifest,
                    family.suffix
                ),
                "case {case:?}: the announced URL must be this service's egress path on the \
                 group's announced address"
            );

            let (host, path) = split_url(&url);
            let denied = emulator.send("GET", &path, None, &host, false, &[]).await;
            assert_eq!(
                denied.status, 403,
                "case {case:?}: a fetch of the announced URL with no read grant expected \
                 HTTP 403, got {} with body {}",
                denied.status, denied.body
            );
            assert!(
                denied.body.to_lowercase().contains("access denied"),
                "case {case:?}: the refusal must say access denied, got {}",
                denied.body
            );
            let granted = control(
                &emulator,
                row.authority,
                "POST",
                &format!(
                    "/channelGroup/{GROUP}/channel/{FAMILY_CHANNEL}/originEndpoint/{endpoint}/policy"
                ),
                Some(json!({"Policy": grant_policy("mediapackagev2:GetObject")})),
            )
            .await;
            assert_eq!(
                granted.status, 200,
                "case {case:?}: PutOriginEndpointPolicy expected 200, got {} with body {}",
                granted.status, granted.body
            );
            let authorized = emulator.send("GET", &path, None, &host, false, &[]).await;
            assert_eq!(
                authorized.status, 404,
                "case {case:?}: an authorized fetch with nothing packaged expected HTTP 404, \
                 got {} with body {}",
                authorized.status, authorized.body
            );
        }
    }
}
