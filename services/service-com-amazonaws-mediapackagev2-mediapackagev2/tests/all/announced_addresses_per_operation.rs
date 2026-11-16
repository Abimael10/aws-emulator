// SPDX-License-Identifier: CC0-1.0

//! That EVERY operation which serializes an address announces one this emulator answers on.
//!
//! RULE under test: the host of every address a response carries — a channel group's
//! `EgressDomain`, a channel's `IngestEndpoints[].Url`, the `Url` of every manifest an origin
//! endpoint publishes, on a create, a read, an update or a listing alike — is the endpoint
//! the request that created the resource reached, and never a host in one of AWS's own
//! endpoint DNS zones: AWS owns those names, this emulator serves nothing on them, and a
//! client that dials one leaves the emulated environment (or fails to resolve it at all).
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

use crate::rejection_rule_and_negative_paths::{split_url, Answer, Emulator};

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
