// SPDX-License-Identifier: CC0-1.0

//! The VPC a function with a `VpcConfig` is ATTACHED to, as a handler running inside it
//! addresses that VPC, end to end over the wire: a `python3.x` function whose code does
//! `import socket`, opens a datagram socket, and reads the address its traffic leaves from and
//! the answers the VPC's own DNS server gives it.
//!
//! The rule pinned here: "Lambda creates an elastic network interface for each subnet in your
//! function's VPC configuration", so a route from inside the run toward an address of that VPC
//! resolves through one of those interfaces — the one in the destination's own subnet when
//! there is one, and otherwise the first the function holds — and the source address the
//! socket reports is that interface's own private address, the one Amazon EC2 handed it out of
//! its subnet's range. The VPC's Amazon-provided DNS server, "at the base of your VPC network
//! range plus two", answers the queries the function sends it: a hostname of an address in the
//! VPC resolves to that address, a reverse question about such an address to that hostname, a
//! name of the VPC's internal zone that names no address in it is `NXDOMAIN`, and a name of
//! another zone — which the resolver would have to recurse for — is `SERVFAIL`. An address
//! OUTSIDE the VPC has no route through the attachment, and a function attached to NO VPC has
//! no route to a VPC address at all.
//!
//! Every VPC, subnet, security group and interface below was minted by Amazon EC2 itself and
//! read back through its own `DescribeNetworkInterfaces`, so the addresses asserted on are the
//! ones the account's own inventory holds rather than values this suite made up. The two
//! services run on one shared engine, exactly as the router mounts them.
//!
//! The rows differ along what the rule generalises over: where the destination sits (each
//! subnet an interface is in, elsewhere in the VPC, the resolver itself, this machine's own
//! interface, an address outside the VPC), which name is asked of the resolver and of which
//! record type, and whether the function is attached to a VPC at all.
//!
//! Each test here RUNS a handler, so it takes the one invocation stream the suites that run
//! handlers share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this
//! emulator serves a bounded number of runs at once, and the memory bound a run is stopped on
//! is read from the whole process, so suites that stand interpreters up in parallel otherwise
//! stop one another's runs.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, zip_file, ROLE,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::vpc_attachment_interfaces::{ec2_client, function_interfaces, network};

/// The second octet of the VPC the rows run in: its range is `10.<prefix>.0.0/16` and its
/// subnets are `10.<prefix>.<index>.0/24`, as the shared network helper mints them.
const PREFIX: u8 = 23;

/// How many subnets the function is attached to, so the route has an interface to choose
/// between.
const SUBNETS: usize = 2;

/// The private DNS domain of the Region every request below is signed for: "AWS uses
/// `ec2.internal` in `us-east-1` and `<region>.compute.internal` in every other Region"
/// (Amazon VPC User Guide, "DHCP option sets"), and the shared harness
/// (`crate::function_lifecycle::REGION`) signs for `us-east-1`.
const INTERNAL_DOMAIN: &str = "ec2.internal";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The DNS record types a probe asks for, as RFC 1035 numbers them.
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;

/// The response codes the resolver answers with, as RFC 1035 numbers them.
const RCODE_NOERROR: u64 = 0;
const RCODE_SERVFAIL: u64 = 2;
const RCODE_NXDOMAIN: u64 = 3;

/// `ENETUNREACH`, the number a host with no route to an address answers with.
const ENETUNREACH: u64 = 101;

/// The handler every row runs: it answers one result per probe the event names — the address a
/// datagram toward an address leaves from, or what the VPC's DNS server replied to a query.
///
/// The DNS messages are framed and read by the handler itself, so what is asserted is a real
/// exchange on the socket rather than a shape this emulator agreed with itself.
const HANDLER_SOURCE: &str = r#"
import socket


def _name_bytes(name):
    labels = [label for label in name.rstrip(".").split(".") if label]
    return b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\x00"


def _query(name, kind, qid):
    header = qid.to_bytes(2, "big") + b"\x01\x00" + b"\x00\x01" + b"\x00\x00" * 3
    return header + _name_bytes(name) + kind.to_bytes(2, "big") + b"\x00\x01"


def _skip_name(data, pos):
    while True:
        length = data[pos]
        if length == 0:
            return pos + 1
        if length & 0xC0:
            return pos + 2
        pos += 1 + length


def _read_name(data, pos):
    labels = []
    while True:
        length = data[pos]
        if length == 0:
            return ".".join(labels) + "."
        if length & 0xC0:
            pos = ((length & 0x3F) << 8) | data[pos + 1]
            continue
        labels.append(data[pos + 1 : pos + 1 + length].decode("ascii"))
        pos += 1 + length


def _answers(reply):
    pos = 12
    for _ in range(int.from_bytes(reply[4:6], "big")):
        pos = _skip_name(reply, pos) + 4
    answered = []
    for _ in range(int.from_bytes(reply[6:8], "big")):
        pos = _skip_name(reply, pos)
        kind = int.from_bytes(reply[pos : pos + 2], "big")
        length = int.from_bytes(reply[pos + 8 : pos + 10], "big")
        start = pos + 10
        pos = start + length
        if kind == 1 and length == 4:
            answered.append(".".join(str(octet) for octet in reply[start:pos]))
        elif kind == 12:
            answered.append(_read_name(reply, start))
    return answered


def _source(host, port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect((host, port))
        bound = sock.getsockname()
        return {"source": bound[0], "port": bound[1]}
    except OSError as failed:
        return {"source": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()


def _resolved(resolver, name, kind, qid):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(5)
    try:
        sock.sendto(_query(name, kind, qid), (resolver, 53))
        reply, peer = sock.recvfrom(512)
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return {
        "id": int.from_bytes(reply[0:2], "big"),
        "rcode": reply[3] & 0x0F,
        "ancount": int.from_bytes(reply[6:8], "big"),
        "answers": _answers(reply),
        "peer": list(peer),
    }


def lambda_handler(event, context):
    results = []
    for index, probe in enumerate(event["probes"]):
        if probe["kind"] == "source":
            results.append(_source(probe["host"], int(probe.get("port") or 53)))
        else:
            results.append(
                _resolved(probe["resolver"], probe["name"], int(probe["type"]), 0x400 + index)
            )
    return {"results": results}
"#;

/// Lambda and the Amazon EC2 service that owns VPCs, subnets and network interfaces, on ONE
/// shared engine, plus an EC2 client over the router's own in-process callback.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; what
    // is asserted here is the attachment, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    Emulator {
        ec2: ec2_client(&callback),
        engine,
        router,
    }
}

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `lambda_function.py`.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the function `name`, attached to `vpc_config` when one is named, and bring its
/// activation forward so it can be invoked — the CLOCK owes that window, so nothing waits.
async fn create_probe_function(emulator: &Emulator, name: &str, vpc_config: Option<Value>) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": HANDLER,
        "Timeout": 30,
        "Code": { "ZipFile": zip_file(&package()) },
    });
    if let Some(vpc_config) = vpc_config {
        body["VpcConfig"] = vpc_config;
    }
    let created = create_function(&emulator.router, &body).await;
    assert_eq!(
        created.status,
        201,
        "expected HTTP 201 from CreateFunction for {name}, got {} ({})",
        created.status,
        created.json()
    );
    make_activation_due(&emulator.engine);
}

/// Invoke `name` with `probes` and answer the results it reported, one per probe.
async fn probed(emulator: &Emulator, name: &str, probes: &[Value]) -> Vec<Value> {
    let answered = json_call(
        &emulator.router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        &json!({ "probes": probes }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "expected HTTP 200 from Invoke of {name}, got {} ({})",
        answered.status,
        answered.json()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "the handler of {name} must run to completion, got {}",
        answered.json()
    );
    answered
        .json()
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("the handler of {name} must report one result per probe"))
}

/// A probe asking which address a datagram toward `host` leaves from.
fn source_probe(host: &str) -> Value {
    json!({ "kind": "source", "host": host, "port": 53 })
}

/// A probe asking the resolver `resolver` about `name`, of record type `kind`.
fn query_probe(resolver: &str, name: &str, kind: u16) -> Value {
    json!({ "kind": "query", "resolver": resolver, "name": name, "type": kind })
}

/// The private address of the function's interface in each subnet, read through EC2's own
/// `DescribeNetworkInterfaces` — keyed by subnet id, so what a route is checked against is the
/// address the account's inventory holds.
async fn addresses_by_subnet(
    ec2: &aws_sdk_ec2::Client,
    function_name: &str,
) -> BTreeMap<String, String> {
    let interfaces = function_interfaces(ec2, function_name).await;
    assert_eq!(
        interfaces.len(),
        SUBNETS,
        "expected one interface per attached subnet, got {}",
        interfaces.len()
    );
    interfaces
        .iter()
        .filter_map(|interface| {
            Some((
                interface.subnet_id()?.to_string(),
                interface.private_ip_address()?.to_string(),
            ))
        })
        .collect()
}

/// RULE: a route from inside a VPC-attached function toward an address of its VPC resolves
/// through the interface in the destination's own subnet, and through the first interface the
/// function holds for an address elsewhere in that VPC; the source address the socket reports
/// is that interface's own private address. This machine's own interface is where the planes
/// this emulator serves in process are, and an address outside the VPC has no route at all.
#[tokio::test]
async fn a_vpc_attached_function_leaves_from_the_address_of_its_own_interface() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let network = network(&emulator.ec2, "in-vpc route", PREFIX, SUBNETS).await;
    let function = "in-vpc-route-probe";
    create_probe_function(
        &emulator,
        function,
        Some(json!({
            "SubnetIds": network.subnet_ids.clone(),
            "SecurityGroupIds": [network.group_id.clone()],
        })),
    )
    .await;
    let addresses = addresses_by_subnet(&emulator.ec2, function).await;
    let in_first_subnet = addresses
        .get(&network.subnet_ids[0])
        .expect("an interface in the first attached subnet")
        .clone();
    let in_second_subnet = addresses
        .get(&network.subnet_ids[1])
        .expect("an interface in the second attached subnet")
        .clone();

    // One case per row: the destination, and the address the route toward it must leave from
    // (`None` for a destination with no route, which is answered ENETUNREACH).
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "an address in the first attached subnet",
            format!("10.{PREFIX}.0.200"),
            Some(in_first_subnet.clone()),
        ),
        (
            "an address in the second attached subnet",
            format!("10.{PREFIX}.1.200"),
            Some(in_second_subnet.clone()),
        ),
        (
            "the VPC's own resolver, which sits in the first subnet",
            format!("10.{PREFIX}.0.2"),
            Some(in_first_subnet.clone()),
        ),
        (
            "an address elsewhere in the VPC",
            format!("10.{PREFIX}.9.9"),
            Some(in_first_subnet.clone()),
        ),
        (
            "this machine's own interface, where the planes of this emulator are served",
            "127.0.0.1".to_string(),
            Some("127.0.0.1".to_string()),
        ),
        (
            "an address of another VPC's range",
            format!("10.{}.0.5", PREFIX + 1),
            None,
        ),
        (
            "a public address",
            "198.51.100.7".to_string(),
            None,
        ),
    ];
    let probes: Vec<Value> = rows
        .iter()
        .map(|(_case, destination, _expected)| source_probe(destination))
        .collect();
    let results = probed(&emulator, function, &probes).await;
    assert_eq!(
        results.len(),
        rows.len(),
        "expected one result per probe, got {results:?}"
    );
    for ((case, destination, expected), result) in rows.iter().zip(results) {
        match expected {
            Some(address) => {
                let reported = result.get("source").and_then(Value::as_str);
                assert_eq!(
                    reported,
                    Some(address.as_str()),
                    "case {case:?}: expected a datagram toward {destination} to leave from \
                     {address}, got {result}"
                );
                let port = result.get("port").and_then(Value::as_u64).unwrap_or_default();
                assert!(
                    port > 0,
                    "case {case:?}: expected the socket to report the port it was lent, got \
                     {result}"
                );
            }
            None => {
                assert_eq!(
                    result.get("source"),
                    Some(&Value::Null),
                    "case {case:?}: expected {destination} to have no route, got {result}"
                );
                assert_eq!(
                    result.get("errno").and_then(Value::as_u64),
                    Some(ENETUNREACH),
                    "case {case:?}: expected errno {ENETUNREACH} toward {destination}, got \
                     {result}"
                );
            }
        }
    }
}

/// RULE: the VPC's own DNS server, at the base of the VPC's range plus two, answers the
/// queries a function attached to that VPC sends it — a hostname of an address in the VPC with
/// that address, a reverse question about such an address with that hostname, a name of the
/// VPC's internal zone that names no address in it with NXDOMAIN, and a name of another zone
/// with SERVFAIL.
#[tokio::test]
async fn the_vpc_resolver_answers_the_queries_a_function_inside_it_sends() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let network = network(&emulator.ec2, "in-vpc resolver", PREFIX, SUBNETS).await;
    let function = "in-vpc-resolver-probe";
    create_probe_function(
        &emulator,
        function,
        Some(json!({
            "SubnetIds": network.subnet_ids.clone(),
            "SecurityGroupIds": [network.group_id.clone()],
        })),
    )
    .await;
    let resolver = format!("10.{PREFIX}.0.2");
    let in_vpc = format!("10.{PREFIX}.1.50");
    let hostname = format!("ip-10-{PREFIX}-1-50.{INTERNAL_DOMAIN}");
    let reverse = format!("50.1.{PREFIX}.10.in-addr.arpa");

    // One case per row: the query, the response code it must be answered with, and the answers
    // it must carry.
    let rows: Vec<(&str, Value, u64, Vec<String>)> = vec![
        (
            "a hostname of an address in the VPC",
            query_probe(&resolver, &hostname, TYPE_A),
            RCODE_NOERROR,
            vec![in_vpc.clone()],
        ),
        (
            "a reverse question about that address",
            query_probe(&resolver, &reverse, TYPE_PTR),
            RCODE_NOERROR,
            vec![format!("{hostname}.")],
        ),
        (
            "a hostname of an address outside the VPC",
            query_probe(
                &resolver,
                &format!("ip-10-{}-0-7.{INTERNAL_DOMAIN}", PREFIX + 1),
                TYPE_A,
            ),
            RCODE_NXDOMAIN,
            Vec::new(),
        ),
        (
            "a name of the internal zone of no hostname form",
            query_probe(&resolver, &format!("broker.{INTERNAL_DOMAIN}"), TYPE_A),
            RCODE_NXDOMAIN,
            Vec::new(),
        ),
        (
            "a name of another zone, which the resolver would have to recurse for",
            query_probe(&resolver, "broker.example.com", TYPE_A),
            RCODE_SERVFAIL,
            Vec::new(),
        ),
    ];
    let probes: Vec<Value> = rows.iter().map(|(_case, probe, _, _)| probe.clone()).collect();
    let results = probed(&emulator, function, &probes).await;
    assert_eq!(
        results.len(),
        rows.len(),
        "expected one result per probe, got {results:?}"
    );
    for ((case, probe, code, answers), result) in rows.iter().zip(results) {
        assert_eq!(
            result.get("rcode").and_then(Value::as_u64),
            Some(*code),
            "case {case:?}: expected response code {code} for {probe}, got {result}"
        );
        let reported: Vec<String> = result
            .get("answers")
            .and_then(Value::as_array)
            .map(|answers| {
                answers
                    .iter()
                    .map(|answer| answer.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            &reported, answers,
            "case {case:?}: expected the answers {answers:?} for {probe}, got {result}"
        );
        assert_eq!(
            result.get("ancount").and_then(Value::as_u64),
            Some(answers.len() as u64),
            "case {case:?}: expected the answer count to agree with the records, got {result}"
        );
        // The reply came back from the resolver's own address, as a datagram's answer does.
        let peer = result
            .get("peer")
            .and_then(Value::as_array)
            .and_then(|peer| peer.first())
            .and_then(Value::as_str);
        assert_eq!(
            peer,
            Some(resolver.as_str()),
            "case {case:?}: expected the reply to come from {resolver}, got {result}"
        );
    }
}

/// RULE: a function attached to NO VPC has no route into one: neither an address of a VPC nor
/// that VPC's resolver is reachable from it, and both are answered the way a host with no
/// route to an address answers it.
#[tokio::test]
async fn a_function_attached_to_no_vpc_reaches_no_vpc_address() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let network = network(&emulator.ec2, "no attachment", PREFIX, SUBNETS).await;
    let function = "no-attachment-probe";
    create_probe_function(&emulator, function, None).await;
    // The VPC exists and has subnets; what this function lacks is an interface in them.
    assert_eq!(
        function_interfaces(&emulator.ec2, function).await.len(),
        0,
        "a function attached to no VPC must hold no interface"
    );
    let resolver = format!("10.{PREFIX}.0.2");
    let probes = vec![
        source_probe(&format!("10.{PREFIX}.0.200")),
        source_probe(&resolver),
        query_probe(&resolver, &format!("ip-10-{PREFIX}-0-200.{INTERNAL_DOMAIN}"), TYPE_A),
    ];
    let cases = [
        "an address in a subnet of the VPC",
        "the VPC's own resolver",
        "a query to that resolver",
    ];
    let results = probed(&emulator, function, &probes).await;
    for (case, result) in cases.iter().zip(results) {
        assert_eq!(
            result.get("errno").and_then(Value::as_u64),
            Some(ENETUNREACH),
            "case {case:?}: expected errno {ENETUNREACH} from a function attached to no VPC, \
             got {result}"
        );
    }
    // Nothing of the VPC the subnets belong to was reached, so its id is reported by EC2 and
    // not by this function.
    assert!(
        !network.vpc_id.is_empty(),
        "the network the rows address must be one EC2 minted"
    );
}
