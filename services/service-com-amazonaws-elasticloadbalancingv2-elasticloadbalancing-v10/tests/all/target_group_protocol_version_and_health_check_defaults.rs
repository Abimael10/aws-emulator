// SPDX-License-Identifier: CC0-1.0

//! What a created target group reports for the three members a create settles from the
//! target group's own protocol — `ProtocolVersion`, `HealthCheckProtocol` and
//! `HealthCheckTimeoutSeconds` — over the wire, and read back through
//! `DescribeTargetGroups` because the value has to be PERSISTED and not only answered once.
//!
//! RULES:
//!
//! 1. A member the request STATES is the member the target group stores: an awsQuery member
//!    carrying nothing states no value, so it takes the default instead of being stored as
//!    a value that matches none the model documents.
//! 2. `ProtocolVersion` belongs to an HTTP/HTTPS target group alone ("[HTTP/HTTPS protocol]
//!    the protocol version"), and such a group always reports one: the version the request
//!    stated, else the documented default `HTTP1` ("the default is HTTP1, which sends
//!    requests to targets using HTTP/1.1"). A target group of any other protocol reports
//!    none.
//! 3. The health check of a target group the request states no `HealthCheckProtocol` for
//!    speaks the target group's OWN protocol wherever a health check can speak it (HTTP and
//!    HTTPS, since "the GENEVE, TLS, UDP, TCP_UDP, QUIC, and TCP_QUIC protocols are not
//!    supported for health checks"), and TCP otherwise; and the timeout such a group takes
//!    is 5 seconds for an Application Load Balancer's protocols and for GENEVE, 10 seconds
//!    for the remaining Network Load Balancer ones (recorded behaviour, which the member's
//!    own "6 seconds"/"10 seconds" sentence disagrees with).
//!
//! The rows span what the rules generalise over: every `ProtocolEnum` value a target group
//! can carry crossed with the three shapes `ProtocolVersion` arrives in (stated, absent, and
//! the member carrying nothing), each of the three `ProtocolVersion` values the member
//! documents, and the health-check protocol and timeout of every target group protocol —
//! including the ones no recorded call cites.
//!
//! The wire harness (`emulator`, `call`, `action`, `p`) is `provisioning_loop`'s; nothing
//! here restates it.

use smithy_server_runtime::MultiServiceRouter;

use crate::provisioning_loop::{action, call, emulator, p, Reply};
use crate::target_group_health_check_schedule::describe_by_name;

/// Every value of `ProtocolEnum` a target group's own `Protocol` is drawn from
/// (`smithy_query(query="shape",shape="ProtocolEnum")`), each with the port its targets
/// receive traffic on, the `ProtocolVersion` such a group reports when the request states
/// none, the protocol its health check then speaks and the timeout that check takes.
const PROTOCOLS: &[ProtocolRow] = &[
    ProtocolRow {
        protocol: "HTTP",
        port: "80",
        protocol_version: Some("HTTP1"),
        health_check_protocol: "HTTP",
        timeout: "5",
    },
    ProtocolRow {
        protocol: "HTTPS",
        port: "443",
        protocol_version: Some("HTTP1"),
        health_check_protocol: "HTTPS",
        timeout: "5",
    },
    ProtocolRow {
        protocol: "TCP",
        port: "1234",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "TLS",
        port: "443",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "UDP",
        port: "53",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "TCP_UDP",
        port: "1234",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "QUIC",
        port: "443",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "TCP_QUIC",
        port: "443",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "10",
    },
    ProtocolRow {
        protocol: "GENEVE",
        port: "6081",
        protocol_version: None,
        health_check_protocol: "TCP",
        timeout: "5",
    },
];

/// One row of [`PROTOCOLS`].
struct ProtocolRow {
    protocol: &'static str,
    port: &'static str,
    /// What the group reports for `ProtocolVersion` when the request states none.
    protocol_version: Option<&'static str>,
    health_check_protocol: &'static str,
    timeout: &'static str,
}

/// Every value `ProtocolVersion` documents ("specify GRPC … specify HTTP2 … the default is
/// HTTP1"), which is the catalog an explicit version is drawn from.
const PROTOCOL_VERSIONS: &[&str] = &["HTTP1", "HTTP2", "GRPC"];

/// Create a target group and answer the reply.
async fn create(
    router: &MultiServiceRouter,
    name: &str,
    fields: &[(&str, &str)],
) -> Reply {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    for (key, value) in fields {
        form.push(p(key, value));
    }
    call(router, &form).await
}

#[tokio::test]
async fn a_target_group_reports_the_protocol_version_of_its_protocol_when_the_request_states_none()
{
    let (router, _engine) = emulator();

    // (the case, the `ProtocolVersion` fields the request carries)
    let unstated: &[(&str, &[(&str, &str)])] = &[
        ("no ProtocolVersion member at all", &[]),
        ("a ProtocolVersion carrying nothing", &[("ProtocolVersion", "")]),
    ];

    for (index, row) in PROTOCOLS.iter().enumerate() {
        for (at, (shape, fields)) in unstated.iter().enumerate() {
            let case = format!("a {} target group with {shape}", row.protocol);
            let name = format!("pv-default-{index}-{at}");
            let mut request: Vec<(&str, &str)> = vec![
                ("Protocol", row.protocol),
                ("Port", row.port),
                ("VpcId", "vpc-12345678"),
            ];
            request.extend_from_slice(fields);
            let reply = create(&router, &name, &request).await;
            assert_eq!(
                reply.status, 200,
                "case {case}: expected the create to be served, got {} {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.text("ProtocolVersion").as_deref(),
                row.protocol_version,
                "case {case}: expected ProtocolVersion {:?}, got {}",
                row.protocol_version,
                reply.body
            );

            // And the value is PERSISTED, not only answered once.
            let described = describe_by_name(&router, &name).await;
            assert_eq!(
                (described.status, described.text("ProtocolVersion").as_deref()),
                (200, row.protocol_version),
                "case {case}: expected the stored ProtocolVersion {:?} to read back, got {}",
                row.protocol_version,
                described.body
            );
        }
    }
}

#[tokio::test]
async fn a_stated_protocol_version_is_the_one_the_target_group_reports() {
    // Rule 1 over the members the model documents: what the request states is what the
    // group stores, for each value of the catalog and each HTTP-side protocol.
    let (router, _engine) = emulator();

    for (index, row) in PROTOCOLS
        .iter()
        .filter(|row| row.protocol_version.is_some())
        .enumerate()
    {
        for (at, version) in PROTOCOL_VERSIONS.iter().enumerate() {
            let case = format!("{version} on a {} target group", row.protocol);
            let name = format!("pv-stated-{index}-{at}");
            let reply = create(
                &router,
                &name,
                &[
                    ("Protocol", row.protocol),
                    ("Port", row.port),
                    ("VpcId", "vpc-12345678"),
                    ("ProtocolVersion", version),
                ],
            )
            .await;
            assert_eq!(
                reply.status, 200,
                "case {case}: expected the create to be served, got {} {}",
                reply.status, reply.body
            );

            let described = describe_by_name(&router, &name).await;
            assert_eq!(
                (described.status, described.text("ProtocolVersion").as_deref()),
                (200, Some(*version)),
                "case {case}: expected the stated version to round-trip, got {}",
                described.body
            );
        }
    }
}

#[tokio::test]
async fn a_health_check_the_request_does_not_configure_follows_the_target_group_protocol() {
    // Rule 3, over every protocol a target group can carry.
    let (router, _engine) = emulator();

    for (index, row) in PROTOCOLS.iter().enumerate() {
        let case = format!("a {} target group taking its health-check defaults", row.protocol);
        let name = format!("hc-default-{index}");
        let reply = create(
            &router,
            &name,
            &[
                ("Protocol", row.protocol),
                ("Port", row.port),
                ("VpcId", "vpc-12345678"),
            ],
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected the create to be served, got {} {}",
            reply.status, reply.body
        );

        let described = describe_by_name(&router, &name).await;
        assert_eq!(
            (
                described.status,
                described.text("HealthCheckProtocol").as_deref(),
                described.text("HealthCheckTimeoutSeconds").as_deref(),
            ),
            (
                200,
                Some(row.health_check_protocol),
                Some(row.timeout)
            ),
            "case {case}: expected the stored health check to be {} / {}, got {}",
            row.health_check_protocol,
            row.timeout,
            described.body
        );
    }
}

#[tokio::test]
async fn a_stated_health_check_protocol_and_timeout_are_what_the_target_group_stores() {
    // Rule 1 for the two health-check members: every pairing of a target group protocol
    // with a health-check protocol its own documentation admits, and a timeout at each
    // published bound of the member's modelled range (2..120) plus one in between.
    let (router, _engine) = emulator();

    // (the target group's protocol, its port, the health-check protocol it admits)
    let pairings: &[(&str, &str, &str)] = &[
        ("HTTP", "80", "HTTP"),
        ("HTTP", "80", "HTTPS"),
        ("HTTPS", "443", "HTTP"),
        ("HTTPS", "443", "HTTPS"),
        ("TCP", "1234", "TCP"),
        ("TCP", "1234", "HTTP"),
        ("TLS", "443", "HTTPS"),
        ("GENEVE", "6081", "TCP"),
    ];
    // Each timeout is paired with an interval that leaves room for it, because a check's
    // timeout may not outlast its interval.
    let timeouts: &[(&str, &str)] = &[("2", "5"), ("37", "60"), ("120", "300")];

    for (index, (protocol, port, health_check_protocol)) in pairings.iter().enumerate() {
        for (at, (timeout, interval)) in timeouts.iter().enumerate() {
            let case =
                format!("a {protocol} target group checked over {health_check_protocol} every \
                         {interval}s with a {timeout}s timeout");
            let name = format!("hc-stated-{index}-{at}");
            let reply = create(
                &router,
                &name,
                &[
                    ("Protocol", protocol),
                    ("Port", port),
                    ("VpcId", "vpc-12345678"),
                    ("HealthCheckProtocol", health_check_protocol),
                    ("HealthCheckTimeoutSeconds", timeout),
                    ("HealthCheckIntervalSeconds", interval),
                ],
            )
            .await;
            assert_eq!(
                reply.status, 200,
                "case {case}: expected the create to be served, got {} {}",
                reply.status, reply.body
            );

            let described = describe_by_name(&router, &name).await;
            assert_eq!(
                (
                    described.status,
                    described.text("HealthCheckProtocol").as_deref(),
                    described.text("HealthCheckTimeoutSeconds").as_deref(),
                    described.text("HealthCheckIntervalSeconds").as_deref(),
                ),
                (
                    200,
                    Some(*health_check_protocol),
                    Some(*timeout),
                    Some(*interval)
                ),
                "case {case}: expected the stated health check to round-trip, got {}",
                described.body
            );
        }
    }
}
