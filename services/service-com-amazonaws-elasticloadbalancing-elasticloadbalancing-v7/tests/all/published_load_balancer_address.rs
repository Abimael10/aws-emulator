// SPDX-License-Identifier: CC0-1.0

//! The address a Classic load balancer PUBLISHES as its `DNSName`, end to end over the wire.
//!
//! RULE: the address an operation publishes for a client to send a resource's own traffic to
//! is an address that client can OPEN A CONNECTION TO as it stands, and it names ONE resource.
//! For a Classic load balancer that address is `CreateAccessPointOutput$DNSName` — "the DNS
//! name of the load balancer", reported again on every `LoadBalancerDescription` and, where it
//! has an alias target, as `CanonicalHostedZoneName` — and what this emulator can hand a
//! client is the `host:port` of the socket it opened for that load balancer: a host in Elastic
//! Load Balancing's own zone resolves to nothing on the machine this emulator runs on, and one
//! host of this emulator's own would be the published address of every load balancer at once,
//! leaving nothing in a connection to say which load balancer it was handed for.
//!
//! So each case here reads the published value and then USES it the way a client does: it
//! opens a TCP connection to the address itself and speaks one HTTP/1.1 request on it, and the
//! answer must be that load balancer's own — the status a Classic load balancer answers when
//! it has no registered instance to send a request to, which an address nothing serves cannot
//! answer at all. The same address is also carried as the `Host` of an unsigned request
//! through the emulator's own front door, which is the other way a client of this emulator
//! reaches it.
//!
//! The rows span what the rule generalises over: how the scheme was named (absent, the
//! explicit `internet-facing`, the explicit `internal`), the region (including one no
//! published table covers), the number of listeners, their protocols and their ports, and the
//! load balancer's own name (short, hyphenated, and the longest a create accepts). The last
//! test is the lifetime half: the address belongs to ONE load balancer and stops answering
//! when that load balancer is deleted, while every other one's keeps answering.
//!
//! The full data-plane behaviour behind the address — which registered instance a request is
//! sent to, and what that instance's own server answers — is `classic_data_plane.rs`.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use super::wire::{
    assert_published_dns_name, call, dialled_authority, emulator_with_engine, kid_text, members,
    param, parse,
};

/// The status a load balancer with no registered instance it may send the request to answers,
/// from the Classic Load Balancer's own `HTTP 503: Service unavailable` — what every case here
/// reads as "this load balancer itself answered".
const NO_INSTANCE: u16 = 503;

/// How long a case waits for a condition the emulator owes — a closed socket refusing a
/// connection — before failing. Generous: it bounds nothing the case measures, and the case
/// looks again for the condition rather than sleeping a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits between two looks at that condition.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// One listener of a case's create request: its protocol, the port the load balancer listens
/// on and the port it forwards traffic to on a registered instance.
#[derive(Debug, Clone, Copy)]
struct ListenerRow {
    protocol: &'static str,
    load_balancer_port: i32,
    instance_port: i32,
}

/// One row of the table below: what it exercises, and the create that exercises it.
#[derive(Debug, Clone, Copy)]
struct PublishedRow {
    what: &'static str,
    region: &'static str,
    name: &'static str,
    /// `None` exercises the documented default (`internet-facing`).
    scheme: Option<&'static str>,
    zones: &'static [&'static str],
    listeners: &'static [ListenerRow],
}

/// The rows every test here drives, spanning what the rule generalises over (see the module
/// docs). `LoadBalancerName` "must have a maximum of 32 characters", and the last row's name is
/// exactly that long, which the issue's own case did not cover.
const ROWS: &[PublishedRow] = &[
    PublishedRow {
        what: "the default scheme with one HTTP listener",
        region: "us-east-1",
        name: "web",
        scheme: None,
        zones: &["us-east-1a"],
        listeners: &[ListenerRow {
            protocol: "HTTP",
            load_balancer_port: 80,
            instance_port: 8080,
        }],
    },
    PublishedRow {
        what: "an explicitly internet-facing load balancer with two listeners",
        region: "eu-west-1",
        name: "public-elb",
        scheme: Some("internet-facing"),
        zones: &["eu-west-1a", "eu-west-1b"],
        listeners: &[
            ListenerRow {
                protocol: "HTTP",
                load_balancer_port: 80,
                instance_port: 80,
            },
            ListenerRow {
                protocol: "TCP",
                load_balancer_port: 9000,
                instance_port: 9001,
            },
        ],
    },
    PublishedRow {
        what: "an internal load balancer whose only listener is a TCP one",
        region: "ap-southeast-2",
        name: "private-elb",
        scheme: Some("internal"),
        zones: &["ap-southeast-2a"],
        listeners: &[ListenerRow {
            protocol: "TCP",
            load_balancer_port: 25,
            instance_port: 2525,
        }],
    },
    PublishedRow {
        what: "the longest name a create accepts, in a region no published table covers",
        region: "us-gov-west-1",
        name: "a-load-balancer-name-of-32-chars",
        scheme: Some("internal"),
        zones: &["us-gov-west-1a", "us-gov-west-1b"],
        listeners: &[ListenerRow {
            protocol: "HTTP",
            load_balancer_port: 8080,
            instance_port: 8080,
        }],
    },
];

/// The create of one row, answering the address it published.
async fn create(router: &MultiServiceRouter, row: &PublishedRow) -> String {
    let mut params = vec![param("LoadBalancerName", row.name)];
    for (index, listener) in row.listeners.iter().enumerate() {
        let member = format!("Listeners.member.{}", index + 1);
        params.push(param(&format!("{member}.Protocol"), listener.protocol));
        params.push(param(
            &format!("{member}.LoadBalancerPort"),
            &listener.load_balancer_port.to_string(),
        ));
        params.push(param(
            &format!("{member}.InstancePort"),
            &listener.instance_port.to_string(),
        ));
    }
    for (index, zone) in row.zones.iter().enumerate() {
        params.push(param(&format!("AvailabilityZones.member.{}", index + 1), zone));
    }
    if let Some(scheme) = row.scheme {
        params.push(param("Scheme", scheme));
    }
    let created = call(router, row.region, "CreateLoadBalancer", &params).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreateLoadBalancer must answer 200, got {}",
        row.what, created.body
    );
    let doc = parse(&created.body);
    kid_text(created.result(&doc), "DNSName").unwrap_or_else(|| {
        panic!(
            "case {:?}: the create must answer a DNSName, got {}",
            row.what, created.body
        )
    })
}

/// What one `DescribeLoadBalancers` reports for `name`: the address it publishes and the alias
/// target beside it.
async fn described(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    params: &[(String, String)],
) -> (Option<String>, Option<String>) {
    let read = call(router, region, "DescribeLoadBalancers", params).await;
    assert_eq!(
        read.status, 200,
        "case {name:?}: DescribeLoadBalancers must answer 200, got {}",
        read.body
    );
    let doc = parse(&read.body);
    let description = members(read.result(&doc), "LoadBalancerDescriptions")
        .into_iter()
        .find(|description| kid_text(description, "LoadBalancerName").as_deref() == Some(name))
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: no description for the load balancer, got {}",
                read.body
            )
        });
    (
        kid_text(description, "DNSName"),
        kid_text(description, "CanonicalHostedZoneName"),
    )
}

/// Send one unsigned HTTP request through the emulator's own front door, carrying `host` as
/// the request's `Host` — which is what a client inside this process does with an address it
/// read off the control plane — and answer the status.
async fn through_the_front_door(router: &MultiServiceRouter, host: &str) -> u16 {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{host}/"))
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the data-plane request is well formed");
    router.dispatch(request).await.status().as_u16()
}

/// Open a TCP connection to `authority` — the address a load balancer published — speak one
/// HTTP/1.1 request on it and answer the status it was answered with, or `None` when nothing
/// accepted the connection.
///
/// This is what a client that holds nothing but `DNSName` does with the value: the address is
/// parsed as published and its port is dialled, with no port of the test's own. The published
/// host has to name the loopback interface the core binds its listeners on — asserted here, so
/// the dial can never leave this machine — and the connection is opened on that interface.
async fn dialled_status(authority: &str, path: &str) -> Option<u16> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let address = authority
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| panic!("case dial {authority:?}: not an address: {error}"));
    assert!(
        address.ip().is_loopback(),
        "case dial {authority:?}: the published address must name the loopback interface this \
         emulator binds its listeners on, got {address:?}"
    );
    let dialled = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, address.port()));
    let mut stream = dialled.await.ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut answered = Vec::new();
    stream.read_to_end(&mut answered).await.ok()?;
    let text = String::from_utf8_lossy(&answered).into_owned();
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
}

/// RULE: every load balancer publishes, as its `DNSName`, the dialable address of the socket
/// opened for it — the same address a describe reports and, for a load balancer that has an
/// alias target, `CanonicalHostedZoneName` carries — and a client that opens a connection to
/// that address as published is answered by that very load balancer.
#[tokio::test]
async fn every_load_balancer_publishes_the_dialable_address_of_its_own_socket() {
    let (router, engine) = emulator_with_engine();
    let mut published: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.what;
        let dns = create(&router, row).await;
        assert_published_dns_name(case, &dns);
        assert_eq!(
            dns,
            dialled_authority(&engine, row.region, row.name),
            "case {case:?}: the published address must be this load balancer's own socket"
        );

        // A client that holds nothing but the published value dials it and is answered by
        // this load balancer: nothing is registered, so what it answers is its own "no
        // instance to send this to".
        let dialled = dialled_status(&dns, "/").await;
        assert_eq!(
            dialled,
            Some(NO_INSTANCE),
            "case {case:?}: a connection to the published address {dns:?} must be answered by \
             the load balancer, got {dialled:?}"
        );
        // And so is a request that carries it as its `Host` through this emulator's own front
        // door, which is the address an in-process client already reaches.
        let front_door = through_the_front_door(&router, &dns).await;
        assert_eq!(
            front_door, NO_INSTANCE,
            "case {case:?}: the published address must be served at the emulator's own \
             endpoint too, got {front_door}"
        );

        // Both reads report the value the create published, and the alias target is the same
        // address — reported only for a load balancer that HAS one, which an `internal` load
        // balancer does not.
        let reads: [(&str, Vec<(String, String)>); 2] = [
            ("named", vec![param("LoadBalancerNames.member.1", row.name)]),
            ("over the region", Vec::new()),
        ];
        for (read, params) in &reads {
            let (reported, alias) = described(&router, row.region, row.name, params).await;
            assert_eq!(
                reported.as_deref(),
                Some(dns.as_str()),
                "case {case:?} ({read}): a describe must report the address the create \
                 published, got {reported:?}"
            );
            let expected_alias = (row.scheme != Some("internal")).then(|| dns.clone());
            assert_eq!(
                alias, expected_alias,
                "case {case:?} ({read}): expected CanonicalHostedZoneName \
                 {expected_alias:?}, got {alias:?}"
            );
        }
        published.push(dns);
    }

    // The address names ONE load balancer, so no two of them share it.
    let distinct: BTreeSet<&String> = published.iter().collect();
    assert_eq!(
        distinct.len(),
        ROWS.len(),
        "each load balancer must publish an address of its own, got {published:?}"
    );
}

/// RULE, the lifetime half: a published address belongs to the load balancer it was minted
/// for — "the DNS name associated with a deleted load balancer is no longer usable" — so the
/// delete stops it answering and leaves every other load balancer's address answering.
#[tokio::test]
async fn a_deleted_load_balancers_published_address_stops_answering() {
    let (router, _engine) = emulator_with_engine();
    let region = ROWS[0].region;
    // Two load balancers of ONE region, each published at an address of its own: one is
    // deleted and the other must be untouched by it.
    let going = PublishedRow {
        what: "the load balancer that is deleted",
        region,
        name: "going-elb",
        ..ROWS[0]
    };
    let staying = PublishedRow {
        what: "the load balancer that stays",
        region,
        name: "staying-elb",
        ..ROWS[0]
    };
    let going_address = create(&router, &going).await;
    let staying_address = create(&router, &staying).await;
    assert_ne!(
        going_address, staying_address,
        "case two load balancers of one region: each must publish an address of its own"
    );

    let deleted = call(
        &router,
        region,
        "DeleteLoadBalancer",
        &[param("LoadBalancerName", going.name)],
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {:?}: DeleteLoadBalancer must answer 200, got {}",
        going.what, deleted.body
    );

    // The delete closes the address: looked for in a loop until nothing answers there, rather
    // than slept for, because closing a socket is the emulator's own work and not a wait this
    // case measures.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut answered = dialled_status(&going_address, "/").await;
    while answered.is_some() && Instant::now() < deadline {
        tokio::time::sleep(SETTLE_POLL).await;
        answered = dialled_status(&going_address, "/").await;
    }
    assert_eq!(
        answered, None,
        "case {:?}: a deleted load balancer's address {going_address:?} must answer nothing, \
         got {answered:?}",
        going.what
    );
    let through = through_the_front_door(&router, &going_address).await;
    assert_ne!(
        through, NO_INSTANCE,
        "case {:?}: no load balancer may answer for a deleted one's address",
        going.what
    );

    // And the load balancer that stays is reached at its own address exactly as before.
    let still = dialled_status(&staying_address, "/").await;
    assert_eq!(
        still,
        Some(NO_INSTANCE),
        "case {:?}: another load balancer's delete must leave this address answering, got \
         {still:?}",
        staying.what
    );
    let (reported, _alias) = described(
        &router,
        region,
        staying.name,
        &[param("LoadBalancerNames.member.1", staying.name)],
    )
    .await;
    assert_eq!(
        reported.as_deref(),
        Some(staying_address.as_str()),
        "case {:?}: a describe must still report its published address, got {reported:?}",
        staying.what
    );
}
