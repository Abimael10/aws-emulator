// SPDX-License-Identifier: CC0-1.0

//! The NAME SERVICE of a function's execution environment, end to end over the wire: a
//! `python3.x` function whose code does `import socket` and resolves the names its event
//! names, through `socket.getaddrinfo` and `socket.gethostbyname`.
//!
//! The rule pinned here: a handler resolves a name against the resolver of the environment it
//! runs in, which answers out of the zones this emulator can name an address in — this
//! machine's own loopback names and the `localhost` domain RFC 6761 §6.3 reserves for it, the
//! private zone of the VPC the function is ATTACHED to (asked of that VPC's own DNS server, so
//! an Amazon-provided hostname resolves to the very address Amazon EC2 handed the function's
//! interface), and the public names the DNS standards themselves fix: the second-level domains
//! RFC 2606 §3 reserves for documentation, which the registry publishes address records for.
//! A name no zone it holds names an address under is answered the way a resolver answers a name
//! that does not exist — `socket.gaierror` carrying `EAI_NONAME` and the sentence a host states
//! for it — and never a refusal that announces this emulator. Resolving a name is not reaching
//! it: what a connection to the resolved address comes to is the route's own answer.
//!
//! The addresses the VPC rows assert on were minted by Amazon EC2 itself and read back through
//! its own `DescribeNetworkInterfaces`, so they are the ones the account's inventory holds
//! rather than values this suite made up. The two services run on one shared engine, exactly
//! as the router mounts them ([`crate::vpc_attachment_interfaces::emulator`], the one harness
//! of this target that stands them both up).
//!
//! The rows differ along what the rule generalises over: the ZONE the name belongs to (this
//! machine, the `localhost` domain, the VPC's internal zone, a documentation domain), the FORM
//! the name takes (an apex, a published host label, a label nothing is published under, a name
//! under each TLD reserved to resolve nowhere), the CALL that resolves it, and whether the
//! function is attached to a VPC at all.
//!
//! Each test here RUNS a handler, so it takes the one invocation stream the suites that run
//! handlers share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::io::Write as _;
use std::net::IpAddr;

use serde_json::{json, Value};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, zip_file, ROLE,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::vpc_attachment_interfaces::{emulator, function_interfaces, network, Emulator};

/// The second octet of the VPC the rows run in: its range is `10.<prefix>.0.0/16` and its one
/// subnet is `10.<prefix>.0.0/24`, as the shared network helper mints them.
const PREFIX: u8 = 29;

/// The private DNS domain of the Region every request below is signed for: "AWS uses
/// `ec2.internal` in `us-east-1`" (Amazon VPC User Guide, "DHCP option sets"), and the shared
/// harness (`crate::function_lifecycle::REGION`) signs for `us-east-1`.
const INTERNAL_DOMAIN: &str = "ec2.internal";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// `EAI_NONAME`, the number a resolver answers a name it holds nothing for with, as
/// `<netdb.h>` numbers it.
const EAI_NONAME: i64 = -2;

/// The whole sentence a host states for that number (`gai_strerror(EAI_NONAME)`), as a Python
/// program reads it off the error — the message carries nothing of the name asked about and
/// nothing of this emulator.
const UNRESOLVED_MESSAGE: &str = "[Errno -2] Name or service not known";

/// The handler every row runs: it answers one result per name the event names, as the two
/// calls a program resolves a name through report it.
const HANDLER_SOURCE: &str = r#"
import socket


def _failure(failed):
    return {
        "resolved": False,
        "error_type": type(failed).__name__,
        "errno": failed.errno,
        "error": str(failed),
    }


def _addresses(name):
    try:
        answered = socket.getaddrinfo(name, 443, socket.AF_UNSPEC, socket.SOCK_STREAM)
    except OSError as failed:
        return _failure(failed)
    return {
        "resolved": True,
        "addresses": sorted(set(entry[4][0] for entry in answered)),
    }


def _one_address(name):
    try:
        return {"resolved": True, "address": socket.gethostbyname(name)}
    except OSError as failed:
        return _failure(failed)


def lambda_handler(event, context):
    results = []
    for name in event["names"]:
        results.append({"getaddrinfo": _addresses(name), "gethostbyname": _one_address(name)})
    return {"results": results}
"#;

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

/// Invoke `name` with `names` and answer what the handler reported for each of them.
async fn resolved(emulator: &Emulator, name: &str, names: &[String]) -> Vec<Value> {
    let answered = json_call(
        &emulator.router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        &json!({ "names": names }),
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
    let results = answered
        .json()
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("the handler of {name} must report one result per name"));
    assert_eq!(
        results.len(),
        names.len(),
        "expected one result per name, got {results:?}"
    );
    results
}

/// What a row expects of a name.
enum Expected {
    /// It resolves to exactly this address.
    Address(String),
    /// It resolves — to whatever addresses the resolver holds for it, which is a value of the
    /// environment and not of the requirement.
    Resolves,
    /// It resolves to nothing: the name does not exist.
    Unresolved,
}

/// Assert what the handler reported for ONE name against what the row expects.
fn assert_resolution(case: &str, name: &str, expected: &Expected, result: &Value) {
    let by_addrinfo = &result["getaddrinfo"];
    let by_name = &result["gethostbyname"];
    match expected {
        Expected::Address(address) => {
            let answered: Vec<String> = by_addrinfo["addresses"]
                .as_array()
                .map(|held| {
                    held.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                answered,
                vec![address.clone()],
                "case {case:?}: expected {name:?} to resolve to {address}, got {by_addrinfo}"
            );
            assert_eq!(
                by_name["address"].as_str(),
                Some(address.as_str()),
                "case {case:?}: expected gethostbyname({name:?}) to answer {address}, got \
                 {by_name}"
            );
        }
        Expected::Resolves => {
            assert_eq!(
                by_addrinfo["resolved"].as_bool(),
                Some(true),
                "case {case:?}: expected {name:?} to resolve inside the sandbox, got \
                 {by_addrinfo}"
            );
            let answered: Vec<&str> = by_addrinfo["addresses"]
                .as_array()
                .map(|held| held.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert!(
                !answered.is_empty(),
                "case {case:?}: expected at least one address for {name:?}, got {by_addrinfo}"
            );
            for address in &answered {
                assert!(
                    address.parse::<IpAddr>().is_ok(),
                    "case {case:?}: expected every answer for {name:?} to be an address, got \
                     {address:?}"
                );
            }
            let single = by_name["address"].as_str().unwrap_or_default();
            assert!(
                single
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_ipv4()),
                "case {case:?}: expected gethostbyname({name:?}) to answer one IPv4 address, \
                 got {by_name}"
            );
        }
        Expected::Unresolved => {
            for (call, reported) in [("getaddrinfo", by_addrinfo), ("gethostbyname", by_name)] {
                assert_eq!(
                    reported["resolved"].as_bool(),
                    Some(false),
                    "case {case:?}: expected {call}({name:?}) to resolve nothing, got {reported}"
                );
                assert_eq!(
                    reported["error_type"].as_str(),
                    Some("gaierror"),
                    "case {case:?}: expected {call}({name:?}) to raise the resolver's own \
                     error, got {reported}"
                );
                assert_eq!(
                    reported["errno"].as_i64(),
                    Some(EAI_NONAME),
                    "case {case:?}: expected {call}({name:?}) to carry EAI_NONAME, got \
                     {reported}"
                );
                assert_eq!(
                    reported["error"].as_str(),
                    Some(UNRESOLVED_MESSAGE),
                    "case {case:?}: expected the sentence a host states for an unresolvable \
                     name, got {reported}"
                );
            }
        }
    }
}

/// RULE: a handler resolves a name against the zones its environment's resolver can name an
/// address in — this machine's names, the private zone of the VPC the function is attached to,
/// and the public names the DNS standards fix — and a name none of them holds an address under
/// raises the error a resolver raises for a name that does not exist.
#[tokio::test]
async fn a_handler_resolves_the_names_its_environment_holds_an_address_for() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let network = network(&emulator.ec2, "sandbox resolution", PREFIX, 1).await;
    let function = "sandbox-resolution-probe";
    create_probe_function(
        &emulator,
        function,
        Some(json!({
            "SubnetIds": network.subnet_ids.clone(),
            "SecurityGroupIds": [network.group_id.clone()],
        })),
    )
    .await;
    // The address Amazon EC2 handed the function's own interface, read back through its own
    // inventory, and the Amazon-provided hostname of that address.
    let interfaces = function_interfaces(&emulator.ec2, function).await;
    let address = interfaces
        .first()
        .and_then(aws_sdk_ec2::types::NetworkInterface::private_ip_address)
        .expect("the attachment stands an interface up with a private address")
        .to_string();
    let hostname = format!("ip-{}.{INTERNAL_DOMAIN}", address.replace('.', "-"));
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "the Amazon-provided hostname of the function's own interface",
            hostname.clone(),
            Expected::Address(address.clone()),
        ),
        (
            "the same hostname in capitals, since a name is compared without regard to case",
            hostname.to_ascii_uppercase(),
            Expected::Address(address.clone()),
        ),
        (
            "the hostname of an address elsewhere in the same VPC",
            format!("ip-10-{PREFIX}-0-200.{INTERNAL_DOMAIN}"),
            Expected::Address(format!("10.{PREFIX}.0.200")),
        ),
        (
            "the hostname of an address outside the VPC",
            format!("ip-10-{}-0-1.{INTERNAL_DOMAIN}", PREFIX + 1),
            Expected::Unresolved,
        ),
        (
            "a name of the internal zone of no hostname form",
            format!("database.{INTERNAL_DOMAIN}"),
            Expected::Unresolved,
        ),
        (
            "this machine's own name",
            "localhost".to_string(),
            Expected::Address("127.0.0.1".to_string()),
        ),
        (
            "a name under the localhost domain",
            "queue.localhost".to_string(),
            Expected::Address("127.0.0.1".to_string()),
        ),
        (
            "a public name the standards publish addresses for",
            "example.com".to_string(),
            Expected::Resolves,
        ),
        (
            "another of them",
            "example.net".to_string(),
            Expected::Resolves,
        ),
        (
            "the host label published under one of them",
            "www.example.org".to_string(),
            Expected::Resolves,
        ),
        (
            "a label nothing is published under",
            "broker.example.com".to_string(),
            Expected::Unresolved,
        ),
        (
            "a name under the TLD reserved to resolve nowhere",
            "no-such-host.invalid".to_string(),
            Expected::Unresolved,
        ),
        (
            "a name under the reserved test TLD",
            "database.test".to_string(),
            Expected::Unresolved,
        ),
        (
            "a name under the reserved example TLD",
            "host.example".to_string(),
            Expected::Unresolved,
        ),
        (
            "a public name no zone of this resolver holds",
            "aws.amazon.com".to_string(),
            Expected::Unresolved,
        ),
    ];
    let names: Vec<String> = rows.iter().map(|(_, name, _)| name.clone()).collect();
    let results = resolved(&emulator, function, &names).await;
    for ((case, name, expected), result) in rows.iter().zip(results) {
        assert_resolution(case, name, expected, &result);
    }
}

/// RULE: the private zone is the zone of the VPC the function is ATTACHED to, so a function
/// attached to none resolves no name of it — while every other zone the resolver holds answers
/// a function attached to nothing exactly as it answers one inside a VPC.
#[tokio::test]
async fn a_function_attached_to_no_vpc_resolves_no_name_of_a_private_zone() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let function = "sandbox-resolution-no-vpc";
    create_probe_function(&emulator, function, None).await;
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "a hostname of the internal zone, which this function has no VPC to resolve in",
            format!("ip-10-{PREFIX}-0-200.{INTERNAL_DOMAIN}"),
            Expected::Unresolved,
        ),
        (
            "this machine's own name",
            "localhost".to_string(),
            Expected::Address("127.0.0.1".to_string()),
        ),
        (
            "a public name the standards publish addresses for",
            "example.com".to_string(),
            Expected::Resolves,
        ),
        (
            "a name under the TLD reserved to resolve nowhere",
            "no-such-host.invalid".to_string(),
            Expected::Unresolved,
        ),
    ];
    let names: Vec<String> = rows.iter().map(|(_, name, _)| name.clone()).collect();
    let results = resolved(&emulator, function, &names).await;
    for ((case, name, expected), result) in rows.iter().zip(results) {
        assert_resolution(case, name, expected, &result);
    }
}
