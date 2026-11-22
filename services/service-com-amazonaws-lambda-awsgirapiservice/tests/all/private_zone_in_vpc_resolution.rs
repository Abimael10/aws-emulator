// SPDX-License-Identifier: CC0-1.0

//! A Route 53 PRIVATE hosted zone, observed the only way one can be: from inside a VPC the
//! zone is associated with, by a handler that asks that VPC's own Amazon-provided resolver.
//!
//! RULE: the Amazon-provided resolver of a VPC answers a query for a name held by a private
//! hosted zone ASSOCIATED with that VPC, whichever of the resolver's two addresses the query
//! was sent to and whichever transport carried it (a datagram, or a connection on the DNS
//! port), and it answers such a name out of no zone the VPC is not associated with — so the
//! association, and only the association, decides what the name resolves to inside the VPC. A
//! name no zone reaching the VPC holds is recursed for, and what the recursion establishes from
//! this emulator is all it can answer: a name under a top-level domain the root zone never
//! delegates does not exist, and any other is one nothing here can answer for.
//!
//! Three crates have to agree for that to hold: Amazon EC2 owns the VPC, its subnet and the
//! interface Lambda attaches the function through, Route 53 owns the hosted zone, its records
//! and its VPC associations, and Lambda runs the handler whose query reaches the resolver.
//! Route 53 Resolver is mounted beside them because it is the service that OWNS the in-VPC DNS
//! data plane, and the answer is read out of the register it records itself in.
//!
//! The rows differ along what the rule generalises over: which of the resolver's two addresses
//! the query is sent to, whether a datagram or a connection carries it, the record type asked
//! for (`A`, `CNAME`), a name the zone holds against one it does not, a name under a top-level
//! domain the root zone never delegates against one under a domain somebody owns, which VPC the
//! query is asked from, and whether the asking VPC is associated with the zone at the time.
//!
//! Every VPC, subnet, security group, hosted zone and record set below was minted by the
//! service that owns it, through that service's own API, so nothing here passes off a record of
//! its own as one the account holds.
//!
//! Each test here RUNS a handler, so it takes the one invocation stream the suites that run
//! handlers share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::io::Write as _;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, zip_file, REGION, ROLE,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::vpc_attachment_interfaces::ec2_client;

/// The module and attribute the deployment package below carries its handler as.
const HANDLER: &str = "index.probe";

/// The LINK-LOCAL address the Amazon-provided DNS server of every VPC also answers at: the
/// server is reached either at "the 169.254.169.253 IP address, or the reserved IP address at
/// the base of the VPC network range plus two"
/// (`ModifyVpcAttributeRequest$EnableDnsSupport`).
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";

/// The DNS record types a probe asks for, as RFC 1035 numbers them.
const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;

/// The response codes the resolver answers with, as RFC 1035 §4.1.1 numbers them.
const RCODE_NOERROR: u64 = 0;
const RCODE_SERVFAIL: u64 = 2;
const RCODE_NXDOMAIN: u64 = 3;

/// The private zone the rows ask about, the names it holds and the addresses they stand for.
///
/// The zone sits under a top-level domain the root zone never delegates (RFC 6761 §6 reserves
/// `test`), which is what a private zone's name usually is: a name nothing outside the VPC
/// could answer.
const ZONE: &str = "probe.example.test";
const APP_NAME: &str = "app.probe.example.test";
const APP_ADDRESS: &str = "10.0.0.7";
const ALIAS_NAME: &str = "www.probe.example.test";
const ABSENT_NAME: &str = "nothing.probe.example.test";

/// A name under ANOTHER never-delegated top-level domain the same section reserves — one no
/// zone of this suite holds, and one the issue behind this suite did not name — and a name
/// under a domain somebody owns and the root zone does delegate. The resolver recurses for
/// both, and what the recursion can establish differs: the first does not exist, the second is
/// one nothing here can answer for.
const NEVER_DELEGATED_NAME: &str = "ledger.nothing-here.invalid";
const DELEGATED_NAME: &str = "broker.example.com";

/// The TTL the record sets below carry, inside `ResourceRecordSet$TTL`'s modelled 0..=172800.
const RECORD_TTL: i64 = 300;

/// The handler every row runs: it asks the resolver address the event names about each probe's
/// name, over the transport the probe names, and reports the reply it read back.
///
/// The DNS messages are framed and read by the handler itself, so what is asserted is a real
/// exchange on a socket inside the VPC rather than a shape this emulator agreed with itself.
const HANDLER_SOURCE: &str = r#"
import socket
import struct

# How long one exchange waits for its reply. Generous for an answer served in this same
# process, and no row measures a span: the invocation's own Timeout bounds every wait besides.
WAIT = 10


def _name_bytes(name):
    labels = [label for label in name.rstrip(".").split(".") if label]
    return b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\x00"


def _query(name, kind, qid):
    header = struct.pack(">HHHHHH", qid, 0x0100, 1, 0, 0, 0)
    return header + _name_bytes(name) + struct.pack(">HH", kind, 1)


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


def _records(reply):
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
            value = ".".join(str(octet) for octet in reply[start:pos])
        else:
            value = _read_name(reply, start)
        answered.append({"type": kind, "value": value})
    return answered


def _read(reply, peer):
    return {
        "id": int.from_bytes(reply[0:2], "big"),
        "rcode": reply[3] & 0x0F,
        "ancount": int.from_bytes(reply[6:8], "big"),
        "records": _records(reply),
        "peer": list(peer),
    }


def _over_udp(resolver, name, kind, qid):
    """One query carried as a DATAGRAM, which is how a resolver is asked by default."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(WAIT)
    try:
        sock.sendto(_query(name, kind, qid), (resolver, 53))
        reply, peer = sock.recvfrom(1024)
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return _read(reply, peer)


def _over_tcp(resolver, name, kind, qid):
    """The same query carried on a CONNECTION to the DNS port, where each message is
    prefixed by its own two-byte length (RFC 1035 section 4.2.2)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(WAIT)
    try:
        sock.connect((resolver, 53))
        asked = _query(name, kind, qid)
        sock.sendall(struct.pack(">H", len(asked)) + asked)
        prefix = b""
        while len(prefix) < 2:
            more = sock.recv(2 - len(prefix))
            if not more:
                raise OSError("the resolver closed the connection before its reply")
            prefix += more
        expected = struct.unpack(">H", prefix)[0]
        reply = b""
        while len(reply) < expected:
            more = sock.recv(expected - len(reply))
            if not more:
                raise OSError("the resolver closed the connection inside its reply")
            reply += more
        peer = sock.getpeername()
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return _read(reply, peer)


def _source(host):
    """The address a datagram toward `host` leaves from, which is the function's own place in
    the VPC."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect((host, 53))
        return {"source": sock.getsockname()[0]}
    except OSError as failed:
        return {"source": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()


def probe(event, context):
    results = []
    for index, asked in enumerate(event["probes"]):
        qid = 0x600 + index
        if asked["kind"] == "source":
            results.append(_source(asked["resolver"]))
        elif asked["kind"] == "tcp":
            results.append(_over_tcp(asked["resolver"], asked["name"], int(asked["type"]), qid))
        else:
            results.append(_over_udp(asked["resolver"], asked["name"], int(asked["type"]), qid))
    return {"results": results}
"#;

/// Lambda, the Amazon EC2 service that owns the VPC the function is attached to, the Route 53
/// service that owns the private hosted zone, and the Route 53 Resolver service that owns the
/// in-VPC DNS data plane — on ONE shared engine, exactly as the router mounts them.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
    route53: aws_sdk_route53::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; what is
    // asserted here is what the resolver answers, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_route53resolver_route53resolver::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    let callback = EmbeddedEmulator::from_router(router.clone());
    Emulator {
        ec2: ec2_client(&callback),
        route53: route53_client(&callback),
        engine,
        router,
    }
}

/// A Route 53 client over `callback`, the in-process seam this emulator routes on — the same
/// construction the EC2 client of these suites takes
/// ([`crate::vpc_attachment_interfaces::ec2_client`]).
fn route53_client(callback: &EmbeddedEmulator) -> aws_sdk_route53::Client {
    let config = aws_sdk_route53::Config::builder()
        .behavior_version(aws_sdk_route53::config::BehaviorVersion::latest())
        .region(aws_sdk_route53::config::Region::new(REGION.to_string()))
        .endpoint_url("https://route53.amazonaws.com")
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_route53::Client::from_conf(config)
}

/// One network a probe function sits in: the VPC, the subnet its interface is placed in, the
/// group it carries, and the address of that VPC's own resolver.
struct Network {
    vpc: String,
    subnet: String,
    group: String,
    /// The second address of the VPC's range, which is where the Amazon-provided DNS server
    /// answers: "the Amazon DNS server … is located at the base of your VPC network range plus
    /// two" (Amazon VPC User Guide, "Amazon DNS server").
    resolver: String,
}

/// Create that network through Amazon EC2's own API.
///
/// The shared helper of these suites ([`crate::vpc_attachment_interfaces::network`]) mints only
/// a `10.<prefix>.0.0/16`, and the rule here generalises over the VPC's own range — so each row
/// names the range it runs in.
async fn network(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    vpc_cidr: &str,
    subnet_cidr: &str,
    group_name: &str,
) -> Network {
    let created = ec2
        .create_vpc()
        .cidr_block(vpc_cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create the VPC: {error:?}"));
    let vpc = created
        .vpc()
        .and_then(aws_sdk_ec2::types::Vpc::vpc_id)
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must report the VPC it minted"))
        .to_string();
    let created = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block(subnet_cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create the subnet: {error:?}"));
    let subnet = created
        .subnet()
        .and_then(aws_sdk_ec2::types::Subnet::subnet_id)
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must report the subnet it minted"))
        .to_string();
    let created = ec2
        .create_security_group()
        .vpc_id(&vpc)
        .group_name(group_name)
        .description("the group the probe function carries")
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create the group: {error:?}"));
    let group = created
        .group_id()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must report the group"))
        .to_string();
    let resolver = resolver_of(case, vpc_cidr);
    Network {
        vpc,
        subnet,
        group,
        resolver,
    }
}

/// The resolver address of an IPv4 range: the base of the range plus two.
fn resolver_of(case: &str, cidr: &str) -> String {
    let base: std::net::Ipv4Addr = cidr
        .split_once('/')
        .map(|(base, _prefix)| base)
        .unwrap_or_else(|| panic!("case {case:?}: the range {cidr:?} must spell a prefix length"))
        .parse()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the range {cidr:?} must spell an address: {error}")
        });
    std::net::Ipv4Addr::from(u32::from(base) + 2).to_string()
}

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `index.py`.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the probe function `name`, attached to `network`, and bring its activation forward so
/// it can be invoked — the CLOCK owes that window, so nothing waits for it.
async fn create_probe_function(emulator: &Emulator, name: &str, network: &Network) {
    let created = create_function(
        &emulator.router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 60,
            "MemorySize": 3008,
            "Code": { "ZipFile": zip_file(&package()) },
            "VpcConfig": {
                "SubnetIds": [network.subnet.clone()],
                "SecurityGroupIds": [network.group.clone()],
            },
        }),
    )
    .await;
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

/// A probe asking `resolver` about `name` of record type `kind`, carried over `transport`.
fn query(transport: &str, resolver: &str, name: &str, kind: u16) -> Value {
    json!({ "kind": transport, "resolver": resolver, "name": name, "type": kind })
}

/// The private hosted zone this suite's names live in, associated with `network`'s VPC at
/// creation, holding one `A` record set and one `CNAME` record set pointing at it.
async fn private_zone(emulator: &Emulator, network: &Network) -> String {
    let created = emulator
        .route53
        .create_hosted_zone()
        .name(ZONE)
        .caller_reference(format!("private-zone-in-vpc-{}", network.vpc))
        .hosted_zone_config(
            aws_sdk_route53::types::HostedZoneConfig::builder()
                .private_zone(true)
                .comment("the zone the probe function resolves out of")
                .build(),
        )
        .vpc(
            aws_sdk_route53::types::Vpc::builder()
                .vpc_id(&network.vpc)
                .vpc_region(aws_sdk_route53::types::VpcRegion::from(REGION))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("Route 53 must create the private zone: {error:?}"));
    let zone_id = created
        .hosted_zone()
        .map(|zone| zone.id().to_string())
        .expect("CreateHostedZone must report the zone it minted");
    let changes = aws_sdk_route53::types::ChangeBatch::builder()
        .changes(record_change(APP_NAME, "A", APP_ADDRESS))
        .changes(record_change(ALIAS_NAME, "CNAME", APP_NAME))
        .build()
        .expect("the change batch must build");
    emulator
        .route53
        .change_resource_record_sets()
        .hosted_zone_id(&zone_id)
        .change_batch(changes)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Route 53 must write the zone's records: {error:?}"));
    zone_id
}

/// One `CREATE` of a record set holding exactly `value`.
fn record_change(name: &str, kind: &str, value: &str) -> aws_sdk_route53::types::Change {
    aws_sdk_route53::types::Change::builder()
        .action(aws_sdk_route53::types::ChangeAction::Create)
        .resource_record_set(
            aws_sdk_route53::types::ResourceRecordSet::builder()
                .name(name)
                .r#type(aws_sdk_route53::types::RrType::from(kind))
                .ttl(RECORD_TTL)
                .resource_records(
                    aws_sdk_route53::types::ResourceRecord::builder()
                        .value(value)
                        .build()
                        .expect("the record must build"),
                )
                .build()
                .expect("the record set must build"),
        )
        .build()
        .expect("the change must build")
}

/// One row of the table: what is asked, and the response code and records the answer must
/// carry.
struct Row {
    case: &'static str,
    /// `"udp"` for a query carried as a datagram, `"tcp"` for one carried on a connection.
    transport: &'static str,
    /// Which of the resolver's two addresses the query is sent to: `true` for the link-local
    /// one, `false` for the base of the VPC's range plus two.
    link_local: bool,
    name: &'static str,
    qtype: u16,
    /// The response code the answer carries.
    rcode: u64,
    /// The records of the answer section, as `(type, value)`.
    records: Vec<(u16, String)>,
}

/// The rows asked from INSIDE the VPC the zone is associated with.
fn rows() -> Vec<Row> {
    let app = vec![(TYPE_A, APP_ADDRESS.to_string())];
    vec![
        Row {
            case: "the zone's A name, as a datagram to the VPC's own resolver",
            transport: "udp",
            link_local: false,
            name: APP_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NOERROR,
            records: app.clone(),
        },
        Row {
            case: "the same name at the link-local address that resolver also answers at",
            transport: "udp",
            link_local: true,
            name: APP_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NOERROR,
            records: app.clone(),
        },
        Row {
            case: "the same name over a connection to the DNS port",
            transport: "tcp",
            link_local: false,
            name: APP_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NOERROR,
            records: app.clone(),
        },
        Row {
            case: "the zone's CNAME name, answered with the alias and what it points at",
            transport: "udp",
            link_local: false,
            name: ALIAS_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NOERROR,
            records: vec![
                (TYPE_CNAME, format!("{APP_NAME}.")),
                (TYPE_A, APP_ADDRESS.to_string()),
            ],
        },
        Row {
            case: "a name under the zone the zone holds no record for",
            transport: "udp",
            link_local: false,
            name: ABSENT_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NXDOMAIN,
            records: Vec::new(),
        },
        Row {
            case: "a name under another top-level domain the root zone never delegates",
            transport: "udp",
            link_local: false,
            name: NEVER_DELEGATED_NAME,
            qtype: TYPE_A,
            rcode: RCODE_NXDOMAIN,
            records: Vec::new(),
        },
        Row {
            case: "a name under a domain somebody owns, which the resolver recurses for",
            transport: "udp",
            link_local: false,
            name: DELEGATED_NAME,
            qtype: TYPE_A,
            rcode: RCODE_SERVFAIL,
            records: Vec::new(),
        },
    ]
}

/// The response code and the `(type, value)` records one reply reports.
fn answered(case: &str, result: &Value) -> (u64, Vec<(u64, String)>) {
    let rcode = result
        .get("rcode")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a reply from the resolver, got {result}"));
    let records = result
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|record| {
            (
                record.get("type").and_then(Value::as_u64).unwrap_or_default(),
                record
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    (rcode, records)
}

/// RULE: a private hosted zone's names answer the resolver of the VPC the zone is associated
/// with — at either address that resolver answers at, over a datagram and over a connection —
/// and answer no resolver of a VPC the zone is not associated with.
#[tokio::test]
async fn a_private_zones_names_answer_inside_the_vpc_it_is_associated_with() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    // The range the row's VPC runs in is an RFC 1918 /16, whose resolver is its `.0.2`.
    let inside = network(
        &emulator.ec2,
        "inside the associated VPC",
        "172.31.0.0/16",
        "172.31.7.0/24",
        "private-zone-inside",
    )
    .await;
    let outside = network(
        &emulator.ec2,
        "inside another VPC",
        "10.44.0.0/16",
        "10.44.1.0/24",
        "private-zone-outside",
    )
    .await;
    private_zone(&emulator, &inside).await;
    create_probe_function(&emulator, "private-zone-inside-probe", &inside).await;
    create_probe_function(&emulator, "private-zone-outside-probe", &outside).await;

    let table = rows();
    let mut probes: Vec<Value> = vec![json!({
        "kind": "source",
        "resolver": inside.resolver.clone(),
    })];
    probes.extend(table.iter().map(|row| {
        let resolver = if row.link_local {
            LINK_LOCAL_RESOLVER.to_string()
        } else {
            inside.resolver.clone()
        };
        query(row.transport, &resolver, row.name, row.qtype)
    }));
    let results = probed(&emulator, "private-zone-inside-probe", &probes).await;
    assert_eq!(
        results.len(),
        probes.len(),
        "expected one result per probe, got {results:?}"
    );

    // The vantage point really is in the VPC: its traffic toward that VPC's resolver leaves
    // from an address of the subnet its interface was placed in.
    let source = results[0]
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case \"the vantage point's own address\": expected an address in 172.31.7.0/24, \
                 got {}",
                results[0]
            )
        })
        .to_string();
    assert!(
        source.starts_with("172.31.7."),
        "case \"the vantage point's own address\": expected an address in 172.31.7.0/24, got \
         {source:?}"
    );

    for (row, result) in table.iter().zip(results.iter().skip(1)) {
        let (rcode, records) = answered(row.case, result);
        assert_eq!(
            rcode, row.rcode,
            "case {:?}: expected response code {}, got {rcode} ({result})",
            row.case, row.rcode
        );
        let expected: Vec<(u64, String)> = row
            .records
            .iter()
            .map(|(kind, value)| (u64::from(*kind), value.clone()))
            .collect();
        assert_eq!(
            records, expected,
            "case {:?}: expected the answer {expected:?}, got {records:?} ({result})",
            row.case
        );
    }

    // And the same name, asked of the resolver of a VPC the zone is NOT associated with, is
    // answered out of no zone of it: nothing of the private zone reaches that VPC, so the
    // resolver recurses — and the name is under a top-level domain the root zone never
    // delegates, so what the recursion establishes is that it does not exist.
    let elsewhere = probed(
        &emulator,
        "private-zone-outside-probe",
        &[query("udp", &outside.resolver, APP_NAME, TYPE_A)],
    )
    .await;
    let case = "the zone's A name asked inside a VPC it is not associated with";
    let (rcode, records) = answered(case, &elsewhere[0]);
    assert_eq!(
        rcode, RCODE_NXDOMAIN,
        "case {case:?}: expected response code {RCODE_NXDOMAIN}, got {rcode} ({})",
        elsewhere[0]
    );
    assert!(
        records.is_empty(),
        "case {case:?}: expected no record, got {records:?}"
    );
}

/// RULE (the other half): the ASSOCIATION decides it — a VPC associated with the zone after the
/// fact resolves its names, and the VPC disassociated from it stops resolving them, with no
/// record set changed either way.
#[tokio::test]
async fn an_association_change_decides_which_vpc_resolves_the_zones_names() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let first = network(
        &emulator.ec2,
        "the VPC the zone was created with",
        "172.31.0.0/16",
        "172.31.9.0/24",
        "private-zone-first",
    )
    .await;
    let second = network(
        &emulator.ec2,
        "the VPC associated after the fact",
        "192.168.0.0/16",
        "192.168.4.0/24",
        "private-zone-second",
    )
    .await;
    let zone_id = private_zone(&emulator, &first).await;
    create_probe_function(&emulator, "association-first-probe", &first).await;
    create_probe_function(&emulator, "association-second-probe", &second).await;

    let asked_in = |network: &Network| vec![query("udp", &network.resolver, APP_NAME, TYPE_A)];

    // Associate the second VPC: the zone's name now answers there too.
    emulator
        .route53
        .associate_vpc_with_hosted_zone()
        .hosted_zone_id(&zone_id)
        .vpc(
            aws_sdk_route53::types::Vpc::builder()
                .vpc_id(&second.vpc)
                .vpc_region(aws_sdk_route53::types::VpcRegion::from(REGION))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("Route 53 must associate the second VPC: {error:?}"));
    for (case, function, network) in [
        (
            "the VPC the zone was created with, once a second is associated",
            "association-first-probe",
            &first,
        ),
        (
            "the VPC associated after the fact",
            "association-second-probe",
            &second,
        ),
    ] {
        let results = probed(&emulator, function, &asked_in(network)).await;
        let (rcode, records) = answered(case, &results[0]);
        assert_eq!(
            rcode, RCODE_NOERROR,
            "case {case:?}: expected response code {RCODE_NOERROR}, got {rcode} ({})",
            results[0]
        );
        assert_eq!(
            records,
            vec![(u64::from(TYPE_A), APP_ADDRESS.to_string())],
            "case {case:?}: expected the zone's own record, got {records:?}"
        );
    }

    // Disassociate the first: the same name, asked from the same place, is no longer answered
    // out of the zone, while the VPC still associated keeps resolving it.
    emulator
        .route53
        .disassociate_vpc_from_hosted_zone()
        .hosted_zone_id(&zone_id)
        .vpc(
            aws_sdk_route53::types::Vpc::builder()
                .vpc_id(&first.vpc)
                .vpc_region(aws_sdk_route53::types::VpcRegion::from(REGION))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("Route 53 must disassociate the first VPC: {error:?}"));
    let case = "the disassociated VPC";
    let results = probed(&emulator, "association-first-probe", &asked_in(&first)).await;
    let (rcode, records) = answered(case, &results[0]);
    assert_eq!(
        rcode, RCODE_NXDOMAIN,
        "case {case:?}: expected response code {RCODE_NXDOMAIN}, got {rcode} ({})",
        results[0]
    );
    assert!(
        records.is_empty(),
        "case {case:?}: expected no record, got {records:?}"
    );
    let case = "the VPC still associated with the zone";
    let results = probed(&emulator, "association-second-probe", &asked_in(&second)).await;
    let (rcode, records) = answered(case, &results[0]);
    assert_eq!(
        rcode, RCODE_NOERROR,
        "case {case:?}: expected response code {RCODE_NOERROR}, got {rcode} ({})",
        results[0]
    );
    assert_eq!(
        records,
        vec![(u64::from(TYPE_A), APP_ADDRESS.to_string())],
        "case {case:?}: expected the zone's own record, got {records:?}"
    );
}
