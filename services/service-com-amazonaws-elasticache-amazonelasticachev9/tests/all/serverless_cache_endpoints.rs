// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINTS a serverless cache publishes, and the poll that reads them back.
//!
//! The rules under test:
//!
//! * a cache `CreateServerlessCache` accepted is in this service's state from that call on,
//!   so the `DescribeServerlessCaches` poll a client runs next answers it — with the
//!   transient status first and the settled one once the clock owes it — instead of
//!   refusing a cache the same session created;
//! * every endpoint that cache publishes names the socket serving THAT role: its writer
//!   endpoint the socket its writes go to and its reader endpoint a socket of its own,
//!   where a session reads what the cache holds and writes nothing. So the two endpoints
//!   are two addresses a client can open a session on, and a cache moved onto another
//!   engine publishes the sockets it is served on then rather than a port nothing listens
//!   at.
//!
//! The tables span what the rules generalise over: the engine the cache runs (one this
//! emulator serves a RESP data plane for, and one it serves nothing for), whether the cache
//! carries a user group, whether the endpoints are read on the create's own answer or on a
//! later poll, and whether the cache was created on the engine it runs or moved onto it.
//! Every assertion goes over the wire — the control plane through the production router, the
//! data plane over the socket the core opened — and nothing reaches into the service's
//! state.

use crate::cache_data_plane::{command, endpoint_of, open_tls, Resp};
use crate::harness::{assert_own_endpoint, params, poll_until, Emulator, Reply, REGION};

/// One row of the endpoint table: the cache to create and what its endpoints must be.
#[derive(Debug, Clone, Copy)]
struct EndpointCase {
    /// What this row is, named in every assertion.
    case: &'static str,
    /// The `ServerlessCacheName` the request sends, which is also the stored identity.
    name: &'static str,
    /// The form pairs sent beside it.
    extra: &'static [(&'static str, &'static str)],
    /// Whether the row attaches a user group — "available for Valkey and Redis OSS only".
    user_group: bool,
    /// Whether this emulator serves the engine's wire: a Valkey / Redis OSS cache is served
    /// a RESP data plane of its own, and a Memcached cache is served nothing, so it keeps
    /// the name ElastiCache's own DNS zone would carry.
    served: bool,
    /// The port the engine's clients connect to, which is what an unserved cache publishes
    /// (Memcached 11211, Valkey / Redis OSS 6379 — `state::default_port`).
    engine_port: &'static str,
}

/// The user group a row that wants one attaches: one user named `default`, usable with no
/// credential, so an unauthenticated session on either endpoint is served the keyspace.
///
/// The RBAC shape itself is another suite's subject (`cache_data_plane`); here the group is
/// only what the fixture's cache carries.
async fn user_group(emulator: &Emulator, id: &str) {
    let created = emulator
        .call(
            "CreateUser",
            &params(&[
                ("UserId", id),
                ("UserName", "default"),
                ("Engine", "redis"),
                ("AccessString", "on ~* +@all"),
                ("NoPasswordRequired", "true"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the group's default user must be created, got {} {}",
        created.status, created.body
    );
    let group = emulator
        .call(
            "CreateUserGroup",
            &params(&[
                ("UserGroupId", id),
                ("Engine", "redis"),
                ("UserIds.member.1", id),
            ]),
        )
        .await;
    assert_eq!(
        group.status, 200,
        "the user group must be created, got {} {}",
        group.status, group.body
    );
    // A cache can only be attached to an ACTIVE group, which the clock owes the group a
    // span after its create: the condition is waited for rather than a time.
    poll_until(
        emulator,
        REGION,
        id,
        "the user group reaching active",
        "DescribeUserGroups",
        &params(&[("UserGroupId", id)]),
        |reply| reply.text("member/Status") == Some("active"),
    )
    .await;
}

/// Create the row's cache and answer the create's own reply.
async fn create(emulator: &Emulator, case: &EndpointCase) -> Reply {
    let mut pairs = params(&[("ServerlessCacheName", case.name)]);
    pairs.extend(params(case.extra));
    if case.user_group {
        user_group(emulator, case.name).await;
        pairs.extend(params(&[("UserGroupId", case.name)]));
    }
    let created = emulator.call("CreateServerlessCache", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {}: CreateServerlessCache must answer 200, got {} {}",
        case.case, created.status, created.body
    );
    created
}

/// Poll `DescribeServerlessCaches` for one cache by name until it reports `available`.
///
/// This is the client's own convergence loop: it looks again until the condition holds, so
/// a cache the create registered is read back and one it did not would fail here.
async fn poll_available(emulator: &Emulator, case: &str, name: &str) -> Reply {
    let described = poll_until(
        emulator,
        REGION,
        case,
        "the serverless cache reaching available",
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", name)]),
        |reply| reply.text("member/Status") == Some("available"),
    )
    .await;
    assert_eq!(
        described.status, 200,
        "case {case}: the poll must be answered 200, got {} {}",
        described.status, described.body
    );
    assert_eq!(
        described.text("ServerlessCacheName"),
        Some(name),
        "case {case}: the poll must report the cache the create registered, got {}",
        described.body
    );
    described
}

/// The writer and the reader endpoint one reply publishes, as `(address, port)` pairs.
fn both_endpoints(reply: &Reply, case: &str) -> ((String, String), (String, String)) {
    (
        endpoint_of(reply, "Endpoint/Address", "Endpoint/Port", case),
        endpoint_of(
            reply,
            "ReaderEndpoint/Address",
            "ReaderEndpoint/Port",
            case,
        ),
    )
}

/// Assert the two endpoints `reply` publishes are what `case` requires of them.
fn assert_endpoints(case: &EndpointCase, where_from: &str, reply: &Reply) {
    let name = case.case;
    let ((writer_address, writer_port), (reader_address, reader_port)) =
        both_endpoints(reply, name);
    if case.served {
        // An endpoint a service advertises is an address the service answers on, and the
        // port is the one its own socket was assigned.
        assert_own_endpoint(
            name,
            &format!("the writer endpoint ({where_from})"),
            &writer_address,
            Some(writer_port.as_str()),
        );
        assert_own_endpoint(
            name,
            &format!("the reader endpoint ({where_from})"),
            &reader_address,
            Some(reader_port.as_str()),
        );
        // Two endpoints, not one reported twice: the read role has a socket of its own, as
        // a serverless cache's reader endpoint does on its own port.
        assert_ne!(
            (writer_address.as_str(), writer_port.as_str()),
            (reader_address.as_str(), reader_port.as_str()),
            "case {name} ({where_from}): the reader endpoint must be an endpoint of its own, \
             got the writer's {writer_address}:{writer_port} twice"
        );
    } else {
        // A cache this emulator serves no wire for keeps the name ElastiCache's own zone
        // would carry and the port the engine's clients connect to.
        for (what, address, port) in [
            ("the writer endpoint", &writer_address, &writer_port),
            ("the reader endpoint", &reader_address, &reader_port),
        ] {
            assert!(
                address.starts_with(&format!("{}-", case.name)),
                "case {name} ({where_from}): {what} must name the cache, got {address:?}"
            );
            assert_eq!(
                port, case.engine_port,
                "case {name} ({where_from}): {what} must carry the engine's client port {}, \
                 got {port}",
                case.engine_port
            );
        }
    }
}

/// Open a session on each endpoint and prove each serves the role it is published for: the
/// writer's socket takes writes, the reader's answers what the cache holds and refuses a
/// write with the engine's own `-READONLY`.
async fn assert_serves_both_roles(case: &str, reply: &Reply, key: &str) {
    let ((writer_address, writer_port), (reader_address, reader_port)) =
        both_endpoints(reply, case);
    let mut writer = open_tls(case, &writer_address, &writer_port).await;
    assert_eq!(
        command(case, &mut writer, &["SET", key, "written-at-the-writer"]).await,
        Resp::Status("OK".to_string()),
        "case {case}: the writer endpoint must take a write"
    );

    let mut reader = open_tls(case, &reader_address, &reader_port).await;
    // ONE keyspace behind both endpoints: a read on the reader answers what the writer
    // wrote, which is what makes it the same cache's reader endpoint.
    assert_eq!(
        command(case, &mut reader, &["GET", key]).await,
        Resp::Bulk("written-at-the-writer".to_string()),
        "case {case}: the reader endpoint must answer what the writer wrote"
    );
    let refused = command(case, &mut reader, &["SET", key, "written-at-the-reader"]).await;
    match &refused {
        Resp::Error(message) => assert!(
            message.starts_with("READONLY "),
            "case {case}: a write on the reader endpoint must be refused READONLY, got \
             {message:?}"
        ),
        other => panic!(
            "case {case}: a write on the reader endpoint must be refused, got {other:?}"
        ),
    }
    // Refused and not applied: the value the writer wrote is still what the cache holds.
    assert_eq!(
        command(case, &mut writer, &["GET", key]).await,
        Resp::Bulk("written-at-the-writer".to_string()),
        "case {case}: a refused write must not have changed the keyspace"
    );
}

#[tokio::test]
async fn a_created_serverless_cache_polls_to_available_on_endpoints_it_answers_on() {
    // The axis: the engine the cache runs (one this emulator serves RESP for, one it serves
    // nothing for), its version line, and whether the cache carries a user group.
    let cases = [
        EndpointCase {
            case: "valkey 9 with a user group",
            name: "endpoint-valkey-cache",
            extra: &[("Engine", "valkey"), ("MajorEngineVersion", "9")],
            user_group: true,
            served: true,
            engine_port: "6379",
        },
        EndpointCase {
            case: "redis on its default version",
            name: "endpoint-redis-cache",
            extra: &[("Engine", "redis")],
            user_group: false,
            served: true,
            engine_port: "6379",
        },
        EndpointCase {
            case: "memcached, an engine this emulator serves no wire for",
            name: "endpoint-memcached-cache",
            extra: &[("Engine", "memcached")],
            user_group: false,
            served: false,
            engine_port: "11211",
        },
    ];

    for case in &cases {
        let emulator = Emulator::new();
        let created = create(&emulator, case).await;
        // The create's own answer already publishes the endpoints the cache is reached at,
        // while it is still `creating`.
        assert_endpoints(case, "create", &created);

        // The poll a client runs next: it converges, because the cache the create accepted
        // is in this service's state from that call on.
        let described = poll_available(&emulator, case.case, case.name).await;
        assert_endpoints(case, "the poll", &described);

        // A read changes nothing a later reader sees: the second poll publishes the same
        // two endpoints as the first.
        let again = poll_available(&emulator, case.case, case.name).await;
        assert_eq!(
            both_endpoints(&again, case.case),
            both_endpoints(&described, case.case),
            "case {}: a second poll must publish the same endpoints",
            case.case
        );

        if case.served {
            assert_serves_both_roles(case.case, &described, "endpoint:key").await;
        }
    }
}

#[tokio::test]
async fn a_serverless_cache_moved_onto_a_served_engine_publishes_the_sockets_it_gains() {
    let emulator = Emulator::new();
    let unserved = EndpointCase {
        case: "a memcached cache before the move",
        name: "moved-cache",
        extra: &[("Engine", "memcached")],
        user_group: false,
        served: false,
        engine_port: "11211",
    };
    create(&emulator, &unserved).await;
    let described = poll_available(&emulator, unserved.case, unserved.name).await;
    assert_endpoints(&unserved, "the poll", &described);

    // "Modifies the engine listed in a serverless cache request": the cache now runs an
    // engine this emulator serves, so it is served — and the endpoints it publishes are
    // the addresses and ports of its own sockets rather than a port nothing listens at.
    let modified = emulator
        .call(
            "ModifyServerlessCache",
            &params(&[
                ("ServerlessCacheName", unserved.name),
                ("Engine", "valkey"),
                ("MajorEngineVersion", "8"),
            ]),
        )
        .await;
    assert_eq!(
        modified.status, 200,
        "the modify must answer 200, got {} {}",
        modified.status, modified.body
    );
    let served = EndpointCase {
        case: "the same cache moved onto valkey",
        served: true,
        engine_port: "6379",
        ..unserved
    };
    assert_endpoints(&served, "the modify", &modified);

    let described = poll_available(&emulator, served.case, served.name).await;
    assert_eq!(
        described.text("Engine"),
        Some("valkey"),
        "the cache must report the engine it was moved onto, got {}",
        described.body
    );
    assert_endpoints(&served, "the poll after the modify", &described);
    assert_serves_both_roles(served.case, &described, "moved:key").await;
}
