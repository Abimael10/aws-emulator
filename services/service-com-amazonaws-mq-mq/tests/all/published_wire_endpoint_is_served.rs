// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a broker publishes for a wire protocol this emulator answers.
//!
//! RULE: an endpoint a control plane publishes for a wire protocol it serves is the
//! dialable address of the socket this emulator bound for that resource — so a client that
//! takes `BrokerInstance$endpoints` verbatim and connects lands on this emulator — and it
//! stays that one address for the life of the resource, on every read of it and across a
//! restart, with every broker publishing the socket of its OWN.
//!
//! What that rules out is a SYNTHESIZED authority: a name composed from the resource's id,
//! this service's label and a Region resolves to nothing a client of this machine can open,
//! so a broker that published one would report a healthy RUNNING data plane at an address
//! pointing away from the emulator.
//!
//! The rows span the engines whose wire this emulator serves and the scheme each publishes
//! that wire under (`state::served_wire`): an ActiveMQ broker's STOMP endpoint and a
//! RabbitMQ broker's AMQP one. Both are TLS from the first byte, so each case reads the
//! published `host:port` out of the DescribeBroker answer, dials the loopback interface
//! that address names and completes the handshake the published scheme promises; a
//! published host that is not an address of this machine fails the case before anything is
//! dialled.
//!
//! The broker reads, the poll loop and the endpoint parsing are the crate's existing test
//! helpers (`crate::broker_lifecycle`, `crate::broker_restart`,
//! `crate::activemq_data_plane`); `tests/all/main.rs` is this binary's crate root, so each
//! is reached through `crate::<suite>`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::net::TcpStream;

use service_com_amazonaws_mq_mq::state::{BROKER_STATE_REBOOT_IN_PROGRESS, BROKER_STATE_RUNNING};

use crate::activemq_data_plane::authority_of;
use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::broker_restart::{poll_until_state, reboot};
use crate::rabbitmq_data_plane::AcceptTheEmulatorsCertificate;

/// The user every broker in this suite is created with.
const USERNAME: &str = "mq-endpoint-user";

/// That user's password.
const PASSWORD: &str = "Str0ng-Endpoint-Pass";

/// One engine whose wire this emulator serves: how a broker of it is created, and the URI
/// scheme the served wire is published under.
struct ServedEngine {
    case: &'static str,
    engine_type: &'static str,
    engine_version: &'static str,
    host_instance_type: &'static str,
    /// The scheme of the endpoint naming the socket this emulator answers for the engine.
    scheme: &'static str,
}

/// The engines this emulator serves a wire for, each with the scheme that wire's endpoint
/// is published under: STOMP over TLS for ActiveMQ and AMQP over TLS for RabbitMQ, which
/// are the two wires `state::served_wire` answers a broker's clients on. The engine and
/// instance-type values are the modelled ones a create accepts for each engine.
const SERVED_ENGINES: &[ServedEngine] = &[
    ServedEngine {
        case: "an ActiveMQ broker's STOMP endpoint",
        engine_type: "ACTIVEMQ",
        engine_version: "5.18",
        host_instance_type: "mq.t3.micro",
        scheme: "stomp+ssl",
    },
    ServedEngine {
        case: "a RabbitMQ broker's AMQP endpoint",
        engine_type: "RABBITMQ",
        engine_version: "3.13",
        host_instance_type: "mq.m5.large",
        scheme: "amqps",
    },
];

/// Create one broker of `engine` and answer its id, without waiting for it: a create is
/// answered before the clock has made the broker RUNNING, so a case that needs several
/// brokers starts them all and polls afterwards.
async fn create(mq: &Mq, engine: &ServedEngine, name: &str) -> String {
    let mut body = create_body(name);
    body["engineType"] = json!(engine.engine_type);
    body["engineVersion"] = json!(engine.engine_version);
    body["deploymentMode"] = json!("SINGLE_INSTANCE");
    body["hostInstanceType"] = json!(engine.host_instance_type);
    body["publiclyAccessible"] = json!(false);
    body["users"] = json!([{ "username": USERNAME, "password": PASSWORD }]);
    let (status, created) = mq.create_broker(body).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected 200 from create, got {status} {created:?}",
        engine.case
    );
    string_at(&created, "/brokerId")
}

/// The one endpoint the broker publishes under `scheme`, read off a DescribeBroker answer.
fn published_endpoint(case: &str, described: &Value, scheme: &str) -> String {
    let instances = described
        .pointer("/brokerInstances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: a RUNNING broker publishes the one instance allocated to it, got \
         {described:?}"
    );
    let endpoints: Vec<String> = instances[0]
        .pointer("/endpoints")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    let matching: Vec<&String> = endpoints
        .iter()
        .filter(|endpoint| endpoint.starts_with(&format!("{scheme}://")))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected one {scheme:?} endpoint, got {endpoints:?}"
    );
    matching[0].clone()
}

/// Assert the published endpoint is an address of THIS machine and that this emulator
/// answers the TLS the published scheme promises on it.
async fn the_endpoint_answers(case: &str, endpoint: &str) {
    let (host, port) = authority_of(case, endpoint);
    // A client resolves the host before it dials: an address of this machine is what it
    // can reach, and a composed name is what it cannot. So the published host has to BE an
    // address, and a loopback one, which is the interface the core binds on by default.
    let address: IpAddr = host.parse().unwrap_or_else(|_| {
        panic!(
            "case {case:?}: the published host must be an address this emulator answers on, got \
             {endpoint:?}"
        )
    });
    assert!(
        address.is_loopback(),
        "case {case:?}: the published host must be an address of this machine, got {endpoint:?}"
    );
    // The loopback interface the published address names, of the family it named it in.
    let tcp = match address {
        IpAddr::V4(_) => TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await,
        IpAddr::V6(_) => TcpStream::connect((Ipv6Addr::LOCALHOST, port)).await,
    };
    let tcp = tcp.unwrap_or_else(|error| {
        panic!("case {case:?}: {endpoint:?} must accept a connection, got {error}")
    });
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a name");
    connector
        .connect(server_name, tcp)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: {endpoint:?} must complete the TLS handshake its scheme promises, \
                 got {error}"
            )
        });
}

/// RULE: the endpoint a broker publishes for a wire this emulator serves is the address of
/// the socket this emulator opened for that broker — a loopback address of this machine
/// that accepts the TLS connection the published scheme promises — the same address on
/// every read of the broker, and a socket of its OWN for every broker.
///
/// The rows span the engines whose wire this emulator serves, and each engine is created
/// TWICE so that what is asserted of the published address holds per broker rather than
/// per engine.
#[tokio::test]
async fn a_published_endpoint_for_a_served_wire_is_the_socket_this_emulator_opened() {
    let mq = Mq::new("us-west-2");

    // Every broker is created first and polled afterwards: the move to RUNNING is owed by
    // the clock, so the brokers come due together rather than one after another.
    let mut brokers: Vec<(&ServedEngine, String, String)> = Vec::new();
    for engine in SERVED_ENGINES {
        for ordinal in 0..2 {
            let name = format!("endpoint-{}-{ordinal}", engine.engine_type.to_lowercase());
            let broker_id = create(&mq, engine, &name).await;
            brokers.push((engine, name, broker_id));
        }
    }

    let mut published_authorities: Vec<(String, String)> = Vec::new();
    for (engine, name, broker_id) in &brokers {
        let case = format!("{} ({name})", engine.case);
        let described = poll_until_state(&mq, broker_id, BROKER_STATE_RUNNING).await;
        let endpoint = published_endpoint(&case, &described, engine.scheme);
        the_endpoint_answers(&case, &endpoint).await;

        // A later read of the same broker publishes the one address it answers on: the
        // endpoint is the socket's, so it does not move between reads.
        let (status, read_again) = mq.describe_broker(broker_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from a second describe, got {status} {read_again:?}"
        );
        let again = published_endpoint(&case, &read_again, engine.scheme);
        assert_eq!(
            again, endpoint,
            "case {case:?}: every read of a broker publishes the one address it answers on, got \
             {again:?} after {endpoint:?}"
        );

        // One socket serves one broker, so no two brokers publish the same authority.
        let (host, port) = authority_of(&case, &endpoint);
        let authority = format!("{host}:{port}");
        if let Some((other, _)) = published_authorities
            .iter()
            .find(|(_, held)| held == &authority)
        {
            panic!(
                "case {case:?}: each broker publishes the socket opened for IT, but {authority:?} \
                 is already published by {other:?}"
            );
        }
        published_authorities.push((case, authority));
    }
}

/// RULE: the address a broker publishes for a served wire belongs to the broker for as
/// long as the broker does — a restart leaves the published endpoint naming the same
/// socket, and that socket still answers once the broker is RUNNING again.
///
/// The rows are again the engines whose wire this emulator serves: a restart is a
/// control-plane transition, so it must not move either engine's data plane.
#[tokio::test]
async fn a_restarted_broker_still_publishes_the_socket_it_answers_on() {
    let mq = Mq::new("eu-west-1");

    let mut brokers: Vec<(&ServedEngine, String)> = Vec::new();
    for engine in SERVED_ENGINES {
        let name = format!("restart-endpoint-{}", engine.engine_type.to_lowercase());
        brokers.push((engine, create(&mq, engine, &name).await));
    }

    let mut before: Vec<String> = Vec::new();
    for (engine, broker_id) in &brokers {
        let case = format!("{} before a restart", engine.case);
        let described = poll_until_state(&mq, broker_id, BROKER_STATE_RUNNING).await;
        let endpoint = published_endpoint(&case, &described, engine.scheme);
        the_endpoint_answers(&case, &endpoint).await;
        before.push(endpoint);
    }

    for (engine, broker_id) in &brokers {
        let (status, code, answer) = reboot(&mq, broker_id).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from reboot, got {status} {code:?} {answer:?}",
            engine.case
        );
        // The restart is observed while it is in flight, so what is read afterwards is the
        // broker that really went through one.
        poll_until_state(&mq, broker_id, BROKER_STATE_REBOOT_IN_PROGRESS).await;
    }

    for ((engine, broker_id), endpoint) in brokers.iter().zip(before.iter()) {
        let case = format!("{} after a restart", engine.case);
        let described = poll_until_state(&mq, broker_id, BROKER_STATE_RUNNING).await;
        let after = published_endpoint(&case, &described, engine.scheme);
        assert_eq!(
            &after, endpoint,
            "case {case:?}: a restart must leave the broker publishing the socket it answers on, \
             got {after:?} after {endpoint:?}"
        );
        the_endpoint_answers(&case, &after).await;
    }
}
