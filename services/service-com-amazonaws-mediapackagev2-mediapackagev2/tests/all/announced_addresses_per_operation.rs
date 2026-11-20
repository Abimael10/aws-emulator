// SPDX-License-Identifier: CC0-1.0

//! That EVERY operation which serializes an address announces one this emulator answers on.
//!
//! RULE under test: the host of every URL a response carries — a channel's
//! `IngestEndpoints[].Url`, the `Url` of every manifest an origin endpoint publishes, on a
//! create, a read, an update or a listing alike — is the endpoint the request that created
//! the resource reached, and never a host in one of AWS's own endpoint DNS zones: AWS owns
//! those names, this emulator serves nothing on them, and a client that dials one leaves the
//! emulated environment (or fails to resolve it at all).
//!
//! A channel group's `EgressDomain` is a DOMAIN rather than a URL, so the same rule reaches
//! it as a NAME: it is announced in the zone that endpoint resolves in, carrying dotted DNS
//! labels and nothing else — no scheme, no path and no `:port`. The tests at the end of this
//! file own that half, over the same address table.
//!
//! The zones are the tree's one catalog of them, called rather than copied
//! (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`: `.amazonaws.com`,
//! `.amazonaws.com.cn`, `.api.aws`, `.on.aws`), so a response is judged against every zone
//! AWS publishes an endpoint in, not only the one a recording happened to show.
//!
//! The rows differ along what the rule generalises over: the ADDRESS a caller reaches this
//! emulator at (an IPv4 loopback address, a loopback name, a bracketed IPv6 literal — each
//! with a port — and a loopback name with no port at all) CROSSED with every operation whose
//! output can carry an address. Each row drives all twelve: the three channel-group
//! operations that report `EgressDomain`, the three channel operations that report
//! `IngestEndpoints`, the four origin-endpoint operations that report manifest URLs, and the
//! two listings whose item shapes carry no address member at all
//! (`ChannelGroupListConfiguration` and `ChannelListConfiguration` declare none —
//! `smithy_query(query="shape",shape="ChannelGroupListConfiguration,ChannelListConfiguration")`),
//! which are swept too so a value appearing there later is caught by the same rule.
//!
//! WHERE the announced address is pinned (the path a local URL carries, the hostname a
//! caller that resolves AWS's own hosts is given) and that the published URLs are SERVED by
//! this service belong to `published_data_plane_addressing`; this suite is the per-operation
//! sweep those two do not do.

use serde_json::{json, Value};

use crate::channel_lifecycle::Emulator as RegionalEmulator;
use crate::rejection_rule_and_negative_paths::{grant_policy, split_url, Answer, Emulator};

/// The names every row's fixtures are created under. One emulator per row, so the names are
/// reused across rows.
const GROUP: &str = "sweep-grp";
const CHANNEL: &str = "sweep-chan";
const ENDPOINT: &str = "sweep-ep";
const MANIFEST: &str = "index";

/// The members an address travels in: a channel group's domain, and the `Url` of an ingest
/// endpoint or of a published manifest. Every operation's output below is searched for them
/// at any depth, so a member added to a list item is swept with the rest.
const ADDRESS_MEMBERS: [&str; 2] = ["EgressDomain", "Url"];

/// One row: the authority a caller reached this emulator at.
#[derive(Debug, Clone, Copy)]
struct Addressing {
    label: &'static str,
    authority: &'static str,
}

/// The rows: the addresses a client on the machine this emulator runs on reaches it at.
const ADDRESSINGS: &[Addressing] = &[
    Addressing {
        label: "an IPv4 loopback address with a port",
        authority: "127.0.0.1:9297",
    },
    Addressing {
        label: "a loopback name with a port",
        authority: "localhost:41235",
    },
    Addressing {
        label: "a bracketed IPv6 literal with a port",
        authority: "[::1]:9297",
    },
    Addressing {
        label: "a loopback name with no port",
        authority: "localhost",
    },
];

/// One operation of the sweep: how it is called, and whether its output must carry an
/// address at all.
struct Call {
    operation: &'static str,
    method: &'static str,
    path: fn() -> String,
    body: fn() -> Option<Value>,
    /// Whether this operation's modelled output carries an address member. The two listings
    /// whose item shapes declare none are swept with `false`: nothing may appear, and
    /// nothing is required to.
    announces: bool,
}

/// Every operation whose output can carry an address, in the order one client walks them.
fn calls() -> Vec<Call> {
    vec![
        Call {
            operation: "CreateChannelGroup",
            method: "POST",
            path: || "/channelGroup".to_string(),
            body: || Some(json!({"ChannelGroupName": GROUP})),
            announces: true,
        },
        Call {
            operation: "GetChannelGroup",
            method: "GET",
            path: || format!("/channelGroup/{GROUP}"),
            body: || None,
            announces: true,
        },
        Call {
            operation: "UpdateChannelGroup",
            method: "PUT",
            path: || format!("/channelGroup/{GROUP}"),
            body: || Some(json!({"Description": "swept"})),
            announces: true,
        },
        Call {
            operation: "ListChannelGroups",
            method: "GET",
            path: || "/channelGroup".to_string(),
            body: || None,
            announces: false,
        },
        Call {
            operation: "CreateChannel",
            method: "POST",
            path: || format!("/channelGroup/{GROUP}/channel"),
            body: || Some(json!({"ChannelName": CHANNEL, "InputType": "CMAF"})),
            announces: true,
        },
        Call {
            operation: "GetChannel",
            method: "GET",
            path: || format!("/channelGroup/{GROUP}/channel/{CHANNEL}/"),
            body: || None,
            announces: true,
        },
        Call {
            operation: "UpdateChannel",
            method: "PUT",
            path: || format!("/channelGroup/{GROUP}/channel/{CHANNEL}/"),
            body: || Some(json!({"Description": "swept"})),
            announces: true,
        },
        Call {
            operation: "ListChannels",
            method: "GET",
            path: || format!("/channelGroup/{GROUP}/channel"),
            body: || None,
            announces: false,
        },
        Call {
            operation: "CreateOriginEndpoint",
            method: "POST",
            path: || format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint"),
            body: || {
                Some(json!({
                    "OriginEndpointName": ENDPOINT,
                    "ContainerType": "TS",
                    "HlsManifests": [{"ManifestName": MANIFEST}],
                }))
            },
            announces: true,
        },
        Call {
            operation: "GetOriginEndpoint",
            method: "GET",
            path: || {
                format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}")
            },
            body: || None,
            announces: true,
        },
        Call {
            operation: "UpdateOriginEndpoint",
            method: "PUT",
            path: || {
                format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}")
            },
            body: || {
                Some(json!({
                    "ContainerType": "TS",
                    "Description": "swept",
                    "HlsManifests": [{"ManifestName": MANIFEST}],
                }))
            },
            announces: true,
        },
        Call {
            operation: "ListOriginEndpoints",
            method: "GET",
            path: || format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint"),
            body: || None,
            announces: true,
        },
    ]
}

/// Send one control-plane request to `authority`: the credential scope is what the service
/// reads its Region from, so every row carries one whatever host it addressed.
async fn control(
    emulator: &Emulator,
    authority: &str,
    call: &Call,
) -> Answer {
    emulator
        .send(
            call.method,
            &(call.path)(),
            (call.body)(),
            authority,
            true,
            &[],
        )
        .await
}

/// Every address `value` carries, at any depth, as `(member, value)` pairs — the members
/// [`ADDRESS_MEMBERS`] names.
fn announced_addresses(value: &Value, member: Option<&str>, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                announced_addresses(child, Some(name), found);
            }
        }
        Value::Array(items) => {
            for item in items {
                announced_addresses(item, member, found);
            }
        }
        Value::String(text) => {
            if let Some(name) = member.filter(|name| ADDRESS_MEMBERS.contains(name)) {
                found.push((name.to_string(), text.clone()));
            }
        }
        _ => {}
    }
}

/// Every string `value` carries, at any depth — what the AWS-zone sweep is applied to, so a
/// host hiding in a member this suite does not know about is caught as well.
fn strings(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for child in map.values() {
                strings(child, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                strings(item, found);
            }
        }
        Value::String(text) => found.push(text.clone()),
        _ => {}
    }
}

/// The host of one announced address: the authority of a URL, or the whole value for a
/// member that carries a bare domain.
fn announced_host(value: &str) -> String {
    if value.starts_with("https://") || value.starts_with("http://") {
        return split_url(value).0;
    }
    value.to_string()
}

#[tokio::test]
async fn every_operation_announces_the_address_the_create_reached() {
    for row in ADDRESSINGS {
        let emulator = Emulator::new();
        for call in calls() {
            let case = format!("{} / {}", row.label, call.operation);
            let answer = control(&emulator, row.authority, &call).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} with body {}",
                answer.status, answer.body
            );

            // No string of any response may name a host in a zone AWS publishes its own
            // endpoints in: such a host is AWS's, not this emulator's.
            let mut every_string = Vec::new();
            strings(&answer.json, &mut every_string);
            for text in &every_string {
                assert!(
                    !::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES
                        .iter()
                        .any(|zone| text.contains(zone)),
                    "case {case:?}: no value may name a host in one of AWS's endpoint DNS \
                     zones, got {text:?}"
                );
            }

            let mut addresses = Vec::new();
            announced_addresses(&answer.json, None, &mut addresses);
            assert_eq!(
                !addresses.is_empty(),
                call.announces,
                "case {case:?}: expected announces={}, got the addresses {addresses:?}",
                call.announces
            );
            for (member, value) in &addresses {
                // The DOMAIN member carries a name, not an authority: it is judged on its
                // form and its zone, since a name may not carry the port a URL does. The
                // tests below own that half of the rule.
                if member == "EgressDomain" {
                    assert_announced_domain(&case, row.authority, value);
                    continue;
                }
                let host = announced_host(value);
                assert_eq!(
                    host, row.authority,
                    "case {case:?}: {member} must be announced on the endpoint the create \
                     reached ({:?}), got {value:?}",
                    row.authority
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The DOMAIN half of the rule: a channel group's `EgressDomain` is a DNS name, never a URL
// authority.
//
// The model documents the member as the domain to integrate "with a downstream CDN (such as
// Amazon CloudFront) or playback device"
// (`smithy_query(query="shape",shape="CreateChannelGroupResponse")`), so the value is a host
// NAME and nothing else: dotted DNS labels, no scheme, no path and no `:port`, for an address
// this emulator answers on. The port this emulator may be reached on belongs to the full URLs
// the group's data plane publishes, which are the members allowed to carry one.
//
// Every path that reports the member (`CreateChannelGroup`, `GetChannelGroup`,
// `UpdateChannelGroup`) reports the one name the create resolved; `ListChannelGroups` is not
// among them, because `ChannelGroupListConfiguration` declares no such member
// (`smithy_query(query="shape",shape="ChannelGroupListConfiguration")`) — which the sweep
// above pins with `announces: false`.
// ---------------------------------------------------------------------------

/// One row of the domain table: the address the creating request reached this emulator at, the
/// Region it was served in, and the name the group is created under.
#[derive(Debug, Clone, Copy)]
struct DomainRow {
    label: &'static str,
    authority: &'static str,
    region: &'static str,
    group: &'static str,
}

/// The rows. The ADDRESS is the axis the rule generalises over; the Region and the shape of the
/// group's own name vary with it, so no case can pass by accident of one Region or one name
/// being special. The last row is the caller that resolves AWS's own hosts, which is announced
/// the hostname this service publishes there.
const DOMAIN_ROWS: &[DomainRow] = &[
    DomainRow {
        label: "an IPv4 loopback address with a port",
        authority: "127.0.0.1:9297",
        region: "us-west-2",
        group: "grp",
    },
    DomainRow {
        label: "a loopback name with a port, and a group name no DNS label may carry",
        authority: "localhost:41235",
        region: "eu-west-1",
        group: "Group_1",
    },
    DomainRow {
        label: "a bracketed IPv6 literal with a port, and the shortest group name",
        authority: "[::1]:9297",
        region: "ap-southeast-2",
        group: "a",
    },
    DomainRow {
        label: "a loopback name with no port, and a long group name",
        authority: "localhost",
        region: "us-east-1",
        group: "a-very-long-channel-group-name-that-still-has-to-fit-in-one-dns-label",
    },
    DomainRow {
        label: "one of AWS's own hostnames, which is a caller that resolves them",
        authority: "mediapackagev2.eu-central-1.amazonaws.com",
        region: "eu-central-1",
        group: "grp",
    },
];

/// That `value` is a DNS domain name and nothing else, naming the case and the value it read.
///
/// `pub(crate)` because it is this crate's ONE reading of the form a domain member must carry:
/// `published_data_plane_addressing`, whose rows report the same member while asserting where
/// the group's URLs are rooted, judges it through here rather than re-stating the rule.
pub(crate) fn assert_is_a_dns_domain(case: &str, value: &str) {
    assert!(
        !value.is_empty(),
        "case {case:?}: EgressDomain must carry a domain name, got {value:?}"
    );
    assert!(
        !value.contains("://") && !value.contains('/'),
        "case {case:?}: EgressDomain must carry no scheme and no path, got {value:?}"
    );
    assert!(
        !value.contains(':'),
        "case {case:?}: EgressDomain must carry no port component, got {value:?}"
    );
    assert!(
        value.parse::<std::net::IpAddr>().is_err() && !value.starts_with('['),
        "case {case:?}: EgressDomain must be a NAME rather than an IP literal, got {value:?}"
    );
    // A DNS name is at most 253 characters of dot-separated labels, each 1..=63 characters of
    // letters, digits and inner hyphens — the alphabet a resolver and a CDN origin accept.
    assert!(
        value.len() <= 253,
        "case {case:?}: EgressDomain must be no longer than a DNS name may be, got {value:?}"
    );
    let labels: Vec<&str> = value.split('.').collect();
    assert!(
        labels.len() >= 2,
        "case {case:?}: EgressDomain must be a dotted domain name, got {value:?}"
    );
    for label in &labels {
        assert!(
            !label.is_empty() && label.len() <= 63,
            "case {case:?}: every label of EgressDomain must be 1..=63 characters, got \
             {value:?}"
        );
        assert!(
            label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-'),
            "case {case:?}: every label of EgressDomain must be lower-case letters, digits \
             and inner hyphens, got {value:?}"
        );
    }
}

/// That `value` is a DNS domain name in the zone the address `authority` resolves in — read
/// through the tree's one reading of that question rather than spelled out per case, with the
/// one exception the addressing makes: a caller that addressed one of AWS's own hosts resolves
/// them, so it is announced the hostname this service publishes there, in AWS's own zone.
///
/// `pub(crate)` for the same reason [`assert_is_a_dns_domain`] is.
pub(crate) fn assert_announced_domain(case: &str, authority: &str, value: &str) {
    assert_is_a_dns_domain(case, value);
    let zone = if ::service_common::data_plane_addressing::addresses_an_aws_endpoint(authority) {
        "amazonaws.com".to_string()
    } else {
        ::service_common::data_plane_addressing::announced_zone_of_authority(authority)
    };
    assert!(
        value.ends_with(&format!(".{zone}")),
        "case {case:?}: EgressDomain must sit in the zone the reached address resolves in \
         ({zone:?}), got {value:?}"
    );
}

/// Create the group for one row and report the `EgressDomain` the create, the read and the
/// update each report.
async fn reported_domains(
    emulator: &RegionalEmulator,
    row: &DomainRow,
) -> Vec<(&'static str, String)> {
    let mut reported = Vec::new();
    for (operation, method, path, body) in [
        (
            "CreateChannelGroup",
            "POST",
            "/channelGroup".to_string(),
            Some(json!({"ChannelGroupName": row.group})),
        ),
        (
            "GetChannelGroup",
            "GET",
            format!("/channelGroup/{}", row.group),
            None,
        ),
        (
            "UpdateChannelGroup",
            "PUT",
            format!("/channelGroup/{}", row.group),
            Some(json!({"Description": "a downstream CDN origin"})),
        ),
    ] {
        let (status, answered) = emulator
            .send_in_region(method, &path, body, row.authority, row.region, &[])
            .await;
        assert_eq!(
            status, 200,
            "case {:?}: {operation} expected HTTP 200, got {status} with body {answered}",
            row.label
        );
        reported.push((
            operation,
            answered
                .get("EgressDomain")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ));
    }
    reported
}

#[tokio::test]
async fn every_path_reports_the_egress_domain_as_a_port_less_dns_name() {
    for row in DOMAIN_ROWS {
        let emulator = RegionalEmulator::new();
        let reported = reported_domains(&emulator, row).await;
        let (_, announced) = &reported[0];
        for (operation, domain) in &reported {
            let case = format!("{} / {operation}", row.label);
            assert_announced_domain(&case, row.authority, domain);
            // The name carries the Region whose resources it serves, exactly as this
            // service's own regional hostnames do.
            assert!(
                domain.split('.').any(|label| label == row.region),
                "case {case:?}: EgressDomain must carry the Region {:?} it serves, got \
                 {domain:?}",
                row.region
            );
            assert_eq!(
                domain, announced,
                "case {case:?}: every path must report the one name the create resolved \
                 ({announced:?}), got {domain:?}"
            );
        }
    }
}

#[tokio::test]
async fn two_groups_of_one_account_are_announced_under_different_names() {
    // The domain is a function of the resource, so one group's name names that group: a
    // client that configures a CDN origin per group must not be handed one host for two.
    let row = DOMAIN_ROWS[0];
    let emulator = RegionalEmulator::new();
    let mut announced: Vec<String> = Vec::new();
    for group in ["first", "second", "third"] {
        let (status, created) = emulator
            .send_in_region(
                "POST",
                "/channelGroup",
                Some(json!({"ChannelGroupName": group})),
                row.authority,
                row.region,
                &[],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {group:?}: CreateChannelGroup expected HTTP 200, got {status} with body \
             {created}"
        );
        let domain = created
            .get("EgressDomain")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_announced_domain(group, row.authority, &domain);
        assert!(
            !announced.contains(&domain),
            "case {group:?}: a second group must be announced under a name of its own, got \
             {domain:?} again among {announced:?}"
        );
        announced.push(domain);
    }
}

#[tokio::test]
async fn the_announced_domain_is_a_host_this_service_answers_on() {
    // The name is only usable if a request addressed to it reaches this service: the
    // emulator's front door resolves no service from the opaque first label, so a name this
    // service did not claim would be answered `404 UnknownService` instead.
    let authority = DOMAIN_ROWS[0].authority;
    let emulator = Emulator::new();
    let created = emulator
        .send(
            "POST",
            "/channelGroup",
            Some(json!({"ChannelGroupName": GROUP})),
            authority,
            true,
            &[],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateChannelGroup expected HTTP 200, got {} with body {}",
        created.status, created.body
    );
    let domain = created.string("EgressDomain").to_string();
    assert_announced_domain("the announced domain", authority, &domain);

    let channel = emulator
        .send(
            "POST",
            &format!("/channelGroup/{GROUP}/channel"),
            Some(json!({"ChannelName": CHANNEL, "InputType": "CMAF"})),
            authority,
            true,
            &[],
        )
        .await;
    assert_eq!(
        channel.status, 200,
        "setup: CreateChannel expected HTTP 200, got {} with body {}",
        channel.status, channel.body
    );
    let endpoint = emulator
        .send(
            "POST",
            &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint"),
            Some(json!({
                "OriginEndpointName": ENDPOINT,
                "ContainerType": "TS",
                "HlsManifests": [{"ManifestName": MANIFEST}],
            })),
            authority,
            true,
            &[],
        )
        .await;
    assert_eq!(
        endpoint.status, 200,
        "setup: CreateOriginEndpoint expected HTTP 200, got {} with body {}",
        endpoint.status, endpoint.body
    );

    // The port the emulator is reached on lives in the published URL, which is the member
    // allowed to carry one — never in the domain asserted above.
    let published = endpoint.json["HlsManifests"][0]["Url"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (url_host, _) = split_url(&published);
    assert_eq!(
        url_host, authority,
        "a published URL carries the authority the create reached, got {published:?}"
    );

    // A fetch addressed to the ANNOUNCED DOMAIN, over this service's own egress path, is
    // answered by this service: refused for want of a grant, and served once one is in place.
    let path = format!("/out/v1/{GROUP}/{CHANNEL}/{ENDPOINT}/{MANIFEST}.m3u8");
    let denied = emulator.send("GET", &path, None, &domain, false, &[]).await;
    assert_eq!(
        denied.status, 403,
        "a fetch at the announced domain with no read grant expected HTTP 403, got {} with \
         body {}",
        denied.status, denied.body
    );
    assert!(
        !denied.body.contains("UnknownService"),
        "the announced domain must be claimed by this service, got {}",
        denied.body
    );
    let granted = emulator
        .send(
            "POST",
            &format!("/channelGroup/{GROUP}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}/policy"),
            Some(json!({"Policy": grant_policy("mediapackagev2:GetObject")})),
            authority,
            true,
            &[],
        )
        .await;
    assert_eq!(
        granted.status, 200,
        "setup: PutOriginEndpointPolicy expected HTTP 200, got {} with body {}",
        granted.status, granted.body
    );
    let authorized = emulator.send("GET", &path, None, &domain, false, &[]).await;
    assert_eq!(
        authorized.status, 404,
        "an authorized fetch at the announced domain with nothing packaged expected HTTP 404, \
         got {} with body {}",
        authorized.status, authorized.body
    );
}
