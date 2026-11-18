// SPDX-License-Identifier: CC0-1.0

//! The in-VPC EXECUTOR a recorded check carries its producer into a cluster's VPC in: a
//! `python3.x` function created with a `VpcConfig` whose deployment package VENDORS a client
//! library, invoked with the selftest that proves the executor usable before anything is
//! measured through it.
//!
//! RULE: a `python3.x` runtime is a promise about the interpreter AND about the standard
//! library that comes with it, so a module of the deployment package — the function's own, or
//! one its package vendors — that imports the waiting, socket, selector or TLS interface
//! imports CLEANLY, and the invocation answers the handler's own payload with no
//! `X-Amz-Function-Error`; and a function attached to a VPC is IN that VPC, so a datagram
//! socket of the run reports a source address toward an address of that VPC which is the
//! private address Amazon EC2 handed the interface Lambda stood up for the attachment. Those
//! two together are what makes such an executor usable: the package runs, and what it says
//! about the network is the account's own record.
//!
//! The rows differ along what the rule generalises over: WHICH interface the vendored module
//! imports (the waiting interface alone, the socket interface and the selectors built on it,
//! the TLS layer over them) and HOW the package carries it (a top-level module, and a package
//! directory with an `__init__.py`). Every row's module is imported by ONE invocation, whose
//! answer also carries the nonce-bound digest this suite computes independently and the source
//! address read back from EC2's own `DescribeNetworkInterfaces`.
//!
//! This test RUNS a handler, so it takes the one invocation stream the suites that run
//! handlers share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).
//!
//! Each half of the rule is pinned on its own elsewhere in this crate —
//! `handler_vendored_module_import.rs` for the import of a vendored module, and
//! `handler_in_vpc_network.rs` for what a VPC-attached run's datagram socket reports — and what
//! is new here is the SELFTEST the recorded check actually makes: ONE invocation that imports
//! the vendored libraries, binds the caller's nonce with a digest and reads its own address in
//! the VPC, which is the shape that decides whether the executor is usable at all.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;

use crate::function_lifecycle::{create_function, json_call, make_activation_due, ROLE};
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than written again here.
use crate::node_invoke_run::package;
use crate::vpc_attachment_interfaces::{ec2_client, function_interfaces, network};

/// The second octet of the VPC this suite runs in: its range is `10.<prefix>.0.0/16`, as the
/// shared network helper mints one.
const PREFIX: u8 = 57;

/// The address the VPC's own DNS server answers at, derived from that same prefix so the two
/// cannot drift: the base of the VPC's range plus two, which is where the recorded check's
/// selftest addresses its own resolver probe.
fn resolver() -> String {
    format!("10.{PREFIX}.0.2")
}

/// The nonce the selftest binds its proof to, as the recorded check's own does: a value the
/// CALLER chooses, so the digest the handler answers can only come from code that really ran.
const NONCE: &str = "9f2c41d7a8b34e05";

/// The version the vendored modules below report, which a client library exposes as
/// `__version__`.
const VENDORED_VERSION: &str = "2.0.6";

/// How many times the invocation is re-driven while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// One vendored module of the package: the case it stands for, the entry it is carried as, the
/// name a handler imports it by, and the source it holds.
struct Vendored {
    case: &'static str,
    entry: &'static str,
    module: &'static str,
    source: &'static str,
}

/// The modules the package vendors, one per row of the rule: each imports a different part of
/// the standard library a client library is built on, and the last is carried as a package
/// directory rather than as a top-level module.
fn vendored() -> Vec<Vendored> {
    vec![
        Vendored {
            case: "a module vendoring the waiting interface alone",
            entry: "waiting_client.py",
            module: "waiting_client",
            source: r#"
"""A vendored client library that multiplexes with the waiting interface."""

import select

__version__ = "2.0.6"


def ready():
    """What this library reports of the platform it is running on."""
    return sorted(name for name in ("poll", "epoll", "select") if hasattr(select, name))
"#,
        },
        Vendored {
            case: "a module vendoring the socket interface and the selectors built on it",
            entry: "socket_client.py",
            module: "socket_client",
            source: r#"
"""A vendored client library that opens its own connections."""

import selectors
import socket

__version__ = "2.0.6"


def ready():
    return sorted(
        name
        for name in ("DefaultSelector", "SelectSelector")
        if hasattr(selectors, name)
    ) + [socket.AF_INET.__class__.__name__]
"#,
        },
        Vendored {
            case: "a package directory vendoring the TLS layer over them",
            entry: "tls_client/__init__.py",
            module: "tls_client",
            source: r#"
"""A vendored client library that secures its own connections."""

import select
import socket
import ssl

__version__ = "2.0.6"


def ready():
    return sorted(name for name in ("SSLContext", "create_default_context") if hasattr(ssl, name))
"#,
        },
    ]
}

/// The handler the selftest invokes: it imports each module the event names, reports what that
/// import came to and what the library says of its platform, binds the caller's nonce with a
/// digest, and reads the source address a datagram toward the VPC's own resolver leaves from.
const HANDLER_SOURCE: &str = r#"
import hashlib
import importlib
import socket


def _imported(module):
    """What importing `module` came to: its version and what it reports, or the failure."""
    try:
        vendored = importlib.import_module(module)
    except BaseException as error:
        return {"error": "%s: %s" % (type(error).__name__, error)}
    return {"version": getattr(vendored, "__version__", None), "ready": vendored.ready()}


def handler(event, context):
    answered = {
        "proof": hashlib.sha256(event["nonce"].encode("utf-8")).hexdigest(),
        "vendored": {module: _imported(module) for module in event["vendored"]},
    }
    opened = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        opened.connect((event["resolver"], 53))
        answered["src_ip"] = opened.getsockname()[0]
    except OSError as error:
        answered["src_ip_error"] = "%s: %s" % (type(error).__name__, error)
    finally:
        opened.close()
    return answered
"#;

/// Lambda and the Amazon EC2 service that owns VPCs, subnets and network interfaces, on ONE
/// shared engine — exactly as the router mounts them — plus an EC2 client over the router's own
/// in-process callback.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; this
    // test asserts what the executor's package and its network report, not the SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
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

/// The body of an answer as text, for a message that says what went wrong.
fn text(answer: &crate::function_lifecycle::Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// RULE, end to end: the executor's package imports every interface a vendored client library
/// is built on and answers the handler's own payload, and the datagram socket of that run
/// reports the private address EC2 holds for the interface the VPC attachment stood up.
#[tokio::test]
async fn a_vpc_attached_executor_runs_its_vendored_client_and_reports_its_own_address() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = emulator();
    let case = "the executor of an in-VPC check";
    let network = network(&emulator.ec2, case, PREFIX, 2).await;
    let function = "in-vpc-executor-selftest";

    // The deployment package: the handler, and one vendored module per row of the rule.
    let vendored = vendored();
    let mut entries: Vec<(&str, &str)> = vec![("index.py", HANDLER_SOURCE)];
    for module in &vendored {
        entries.push((module.entry, module.source));
    }
    let created = create_function(
        &emulator.router,
        &json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": 25,
            "VpcConfig": {
                "SubnetIds": network.subnet_ids,
                "SecurityGroupIds": [network.group_id],
            },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&entries)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup CreateFunction: expected HTTP 201, got {} {}",
        created.status,
        text(&created)
    );
    // The activation the create owes is the CLOCK's, so it is brought forward rather than
    // waited out.
    make_activation_due(&emulator.engine);

    // The address the account's own inventory holds for the interface the attachment stood
    // up, read through EC2's own DescribeNetworkInterfaces.
    let interfaces = function_interfaces(&emulator.ec2, function).await;
    let held: Vec<String> = interfaces
        .iter()
        .filter_map(|interface| interface.private_ip_address().map(str::to_string))
        .collect();
    assert!(
        !held.is_empty(),
        "setup: EC2 must hold an interface with a private address for {function:?}"
    );

    let event = json!({
        "nonce": NONCE,
        "resolver": resolver(),
        "vendored": vendored.iter().map(|module| module.module).collect::<Vec<_>>(),
    });
    let uri = format!("/2015-03-31/functions/{function}/invocations");
    let mut answer = json_call(&emulator.router, "POST", &uri, &event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &text(&answer)) {
            break;
        }
        answer = json_call(&emulator.router, "POST", &uri, &event).await;
    }
    assert!(
        !is_host_bound(answer.status, &text(&answer)),
        "every attempt reported a bound this whole test binary shares, so nothing of the rule \
         was read: {}",
        text(&answer)
    );
    assert_eq!(
        answer.status,
        200,
        "expected HTTP 200 from Invoke, got {} {}",
        answer.status,
        text(&answer)
    );
    // The selftest is only evidence if the RUN produced it: no function error, and the digest
    // bound to the nonce this suite chose.
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "the package imports its vendored modules and runs, so the invocation carries no \
         function error, got {failure:?} with {}",
        text(&answer)
    );
    let reported = answer.json();
    let expected_proof = format!("{:x}", Sha256::digest(NONCE.as_bytes()));
    assert_eq!(
        reported.get("proof").and_then(Value::as_str),
        Some(expected_proof.as_str()),
        "expected the nonce-bound digest {expected_proof}, got {reported}"
    );

    // Every vendored module imported cleanly, reported its own version, and answered what it
    // reads off the platform.
    for module in &vendored {
        let imported = &reported["vendored"][module.module];
        assert_eq!(
            imported.get("error"),
            None,
            "case {:?}: the vendored module must import, got {imported}",
            module.case
        );
        assert_eq!(
            imported.get("version").and_then(Value::as_str),
            Some(VENDORED_VERSION),
            "case {:?}: expected the vendored version {VENDORED_VERSION}, got {imported}",
            module.case
        );
        let ready = imported
            .get("ready")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert!(
            !ready.is_empty(),
            "case {:?}: the vendored module must read its platform's own names, got {imported}",
            module.case
        );
    }

    // And the run's own address inside the VPC: the one EC2 holds for the attachment's
    // interface, not a value the handler or this suite made up.
    let source = reported
        .get("src_ip")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "the executor must report the address its datagrams leave from, got {reported}"
            )
        });
    assert!(
        held.iter().any(|address| address == source),
        "expected one of the interface addresses EC2 holds {held:?}, got {source:?} in \
         {reported}"
    );
}
