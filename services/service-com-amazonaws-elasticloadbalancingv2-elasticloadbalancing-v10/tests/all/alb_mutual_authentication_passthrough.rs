// SPDX-License-Identifier: CC0-1.0

//! An HTTPS listener in MUTUAL-AUTHENTICATION PASSTHROUGH mode, end to end over a real
//! handshake: what the TARGET it forwards to is told about the certificate the client
//! presented.
//!
//! The rule under test: **a listener whose `MutualAuthentication$Mode` is `passthrough` asks
//! every client for a certificate and states the one it presented on the request it forwards —
//! the whole chain, the leaf, and the leaf's subject, issuer, validity and serial number —
//! under the names its own attribute collection gives them; every other listener, and every
//! connection that presented nothing, hands the target none of them. A value the CLIENT sent
//! under one of those names never reaches the target.**
//!
//! Everything is driven through the owning APIs: the certificate the listener presents is
//! imported through Certificate Manager, the target is a function created through Lambda's own
//! API and registered through this service's `RegisterTargets`, the mode and the header names
//! are written with `ModifyListener` and `ModifyListenerAttributes`, and the request is sent
//! over a REAL TLS connection to the address the load balancer published — so the handshake
//! that asks for the certificate is in the path. What the target received is read back from
//! the target itself: the function answers with the headers of this family the event carried,
//! so a row that asserts a header is asserting what the function was really handed.
//!
//! The rows span what the rule generalises over: the listener's own mode (`passthrough`, then
//! `off`), whether the client presents a certificate at all, whether it forges a value of its
//! own under one of the names, and whether the listener renames the member carrying the chain.
//!
//! The TRACE the load balancer stamps on every request it forwards
//! ([`service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::service::request_tracing`])
//! is read off the same echo: a target is told which chain the load balancer served its
//! request on, whatever the listener does with a client certificate.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::alb_https_listener_tls::{client_config_presenting, dial, mint, Minted};
use crate::alb_lambda_target_published_address::{
    activate_functions, execution_role, lambda, HANDLER, RUNTIME,
};
use crate::provisioning_loop::{action, call, p};

/// The function the listener forwards to, the role it runs as, and the name the load
/// balancer's certificate covers.
const FUNCTION: &str = "alb-mtls-echo";
const ROLE_NAME: &str = "alb-mtls-echo-exec";
const SERVER_NAME: &str = "mtls.example.com";

/// The name the client's own certificate is issued to, which is what the subject and the
/// issuer of a self-signed leaf both state.
const CLIENT_NAME: &str = "mtls-client.example.com";

/// What a forging client writes under a name of the family, and which name it writes it
/// under: the member that carries the whole chain, the one an application reads.
const FORGED: &str = "forged-by-the-client";
const CHAIN_HEADER: &str = "x-amzn-mtls-clientcert";

/// The name a row renames that member to, as a caller writes one into the listener's own
/// `routing.http.request.x_amzn_mtls_clientcert.header_name`.
const RENAMED_CHAIN_HEADER: &str = "x-client-chain";
const CHAIN_HEADER_NAME_ATTRIBUTE: &str = "routing.http.request.x_amzn_mtls_clientcert.header_name";

/// The port the HTTPS listener is created on, and the mode each phase sets.
const LISTENER_PORT: &str = "443";
const PASSTHROUGH: &str = "passthrough";
const OFF: &str = "off";

/// The name the trace identifier travels under, as the one reading of it this workspace has
/// names it.
const TRACE_HEADER: &str = "x-amzn-trace-id";

/// How long one dial and one exchange may take. Generous on purpose: everything waited for
/// happens in this process, so a loaded host does not decide a verdict.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The function the rows forward to: it answers with the headers of this family the event
/// carried, so what the TARGET received is what a row reads.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    seen = {
        name.lower(): value
        for name, value in headers.items()
        if "mtls-clientcert" in name.lower()
        or name.lower() in ("x-client-chain", "x-amzn-trace-id")
    }
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(seen, sort_keys=True),
    }
"#;

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `lambda_function.py`, written in
/// memory the way a caller uploads one.
fn package() -> Vec<u8> {
    use std::io::Write as _;

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

/// This service, Certificate Manager, Lambda, IAM and the token service over ONE shared
/// engine, as the assembled router mounts them: the certificate the listener presents is
/// Certificate Manager's resource, and the target is a function Lambda runs as a session of
/// the role IAM holds.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// Dial the published address, complete a TLS handshake presenting `client` (or presenting
/// nothing), send one request carrying `forged` where a row forges one, and answer the
/// headers of the family the TARGET reported.
async fn asked(
    case: &str,
    published: &str,
    client: Option<&Minted>,
    forged: Option<(&str, &str)>,
) -> Value {
    let conn = dial(published).await;
    let connector = tokio_rustls::TlsConnector::from(client_config_presenting(client));
    let asked_for = rustls::pki_types::ServerName::from(std::net::Ipv4Addr::LOCALHOST);
    let mut stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connector.connect(asked_for, conn))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the handshake did not complete in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
    let forged = forged
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET /echo HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\n{forged}\
         Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the listener did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    let raw = String::from_utf8_lossy(&answer).to_string();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
    assert_eq!(
        status, 200,
        "case {case:?}: expected the target's own 200, got {status} in {raw:?}"
    );
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_head, body)| body.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no body: {raw:?}"));
    serde_json::from_str(body.trim())
        .unwrap_or_else(|error| panic!("case {case:?}: the target reported {body:?}: {error}"))
}

/// Set the listener's mutual-authentication mode through `ModifyListener`.
async fn set_mode(router: &MultiServiceRouter, case: &str, listener_arn: &str, mode: &str) {
    let mut form = action("ModifyListener");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("MutualAuthentication.Mode", mode));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ModifyListener, got {} body {}",
        reply.status, reply.body
    );
}

/// Write — or clear — the attribute that renames the member carrying the whole chain.
async fn set_chain_header_name(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
    name: &str,
) {
    let mut form = action("ModifyListenerAttributes");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Attributes.member.1.Key", CHAIN_HEADER_NAME_ATTRIBUTE));
    form.push(p("Attributes.member.1.Value", name));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ModifyListenerAttributes, got {} body {}",
        reply.status, reply.body
    );
}

/// One row: the listener's mode, whether the client presents its certificate, whether it
/// forges a value of its own, the name the chain must travel under, and whether the target is
/// told anything at all.
struct Row {
    case: &'static str,
    mode: &'static str,
    presents: bool,
    forges: bool,
    chain_header: &'static str,
    told: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a passthrough listener whose client presented a certificate",
        mode: PASSTHROUGH,
        presents: true,
        forges: false,
        chain_header: CHAIN_HEADER,
        told: true,
    },
    Row {
        case: "the same client forging a chain of its own",
        mode: PASSTHROUGH,
        presents: true,
        forges: true,
        chain_header: CHAIN_HEADER,
        told: true,
    },
    Row {
        case: "a client that presented nothing and forged a chain",
        mode: PASSTHROUGH,
        presents: false,
        forges: true,
        chain_header: CHAIN_HEADER,
        told: false,
    },
    Row {
        case: "a listener whose mutual authentication is off",
        mode: OFF,
        presents: true,
        forges: true,
        chain_header: CHAIN_HEADER,
        told: false,
    },
    Row {
        case: "a passthrough listener that renames the member carrying the chain",
        mode: PASSTHROUGH,
        presents: true,
        forges: true,
        chain_header: RENAMED_CHAIN_HEADER,
        told: true,
    },
];

/// RULE: a `passthrough` listener states the certificate the client presented on the request
/// it forwards, under the names its own attributes give them, and hands the target nothing a
/// client wrote under one of those names.
#[tokio::test]
async fn a_passthrough_listener_hands_its_target_the_certificate_the_client_presented() {
    let (router, engine) = emulator();

    // The target: a function created through Lambda's own API, running as a role IAM holds.
    let role = execution_role(
        &router,
        ROLE_NAME,
        // The function makes no call of its own, so its one grant names a resource it never
        // reaches; what matters is that it runs as a role the account really holds.
        "arn:aws:sqs:us-west-2:123456789012:alb-mtls-echo-unused",
    )
    .await;
    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    activate_functions(&engine);

    // The certificate the listener presents, imported through Certificate Manager, and the
    // certificate the CLIENT presents, which no service of this account holds: it is the
    // client's own, and passthrough is the mode that passes it on without judging it.
    let server = mint(&[SERVER_NAME]);
    let certificate_arn = crate::alb_https_listener_tls::import(&router, "setup", &server).await;
    let client = mint(&[CLIENT_NAME]);

    let (lb_arn, published) = create_load_balancer(&router, "mtls-passthrough-alb").await;

    let mut form = action("CreateTargetGroup");
    form.push(p("Name", "mtls-passthrough-tg"));
    form.push(p("TargetType", "lambda"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateTargetGroup: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let tg_arn = reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("setup CreateTargetGroup: no TargetGroupArn in {}", reply.body));

    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", &tg_arn));
    form.push(p("Targets.member.1.Id", &function_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup RegisterTargets: expected 200, got {} body {}",
        reply.status, reply.body
    );

    let (status, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        json!({
            "StatementId": "elbv2-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": tg_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected 201, got {status} {granted}"
    );

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", LISTENER_PORT));
    form.push(p("Certificates.member.1.CertificateArn", &certificate_arn));
    form.push(p("MutualAuthentication.Mode", PASSTHROUGH));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    // What the first row's target was told, which the forging rows are compared against: the
    // chain the load balancer states is the client's own certificate, not a client's value.
    let mut stated_chain: Option<String> = None;

    for row in ROWS {
        set_mode(&router, row.case, &listener_arn, row.mode).await;
        set_chain_header_name(&router, row.case, &listener_arn, row.chain_header).await;
        let reported = asked(
            row.case,
            &published,
            row.presents.then_some(&client),
            row.forges.then_some((CHAIN_HEADER, FORGED)),
        )
        .await;
        let reported = reported
            .as_object()
            .unwrap_or_else(|| panic!("case {:?}: the target reported {reported}", row.case))
            .clone();
        // Whatever the listener does with a client certificate, the target is told which
        // chain the load balancer served its request on.
        let traced = reported
            .get(TRACE_HEADER)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the target to be told the trace of the request, got \
                     {reported:?}",
                    row.case
                )
            });
        assert!(
            traced.starts_with("Root=1-"),
            "case {:?}: expected the chain the request belongs to, got {traced:?}",
            row.case
        );
        let reported: ::serde_json::Map<String, Value> = reported
            .into_iter()
            .filter(|(name, _)| name != TRACE_HEADER)
            .collect();
        if !row.told {
            assert!(
                reported.is_empty(),
                "case {:?}: expected the target to be told nothing of a client certificate, \
                 got {reported:?}",
                row.case
            );
            continue;
        }
        let chain = reported
            .get(row.chain_header)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the chain under {:?}, got {reported:?}",
                    row.case, row.chain_header
                )
            })
            .to_string();
        assert_ne!(
            chain, FORGED,
            "case {:?}: the chain a target is told is never a client's own value",
            row.case
        );
        assert!(
            chain.starts_with("-----BEGIN%20CERTIFICATE-----"),
            "case {:?}: expected the URL-encoded chain, got {chain:?}",
            row.case
        );
        match stated_chain.as_ref() {
            Some(first) => assert_eq!(
                &chain, first,
                "case {:?}: the same client presents the same chain",
                row.case
            ),
            None => stated_chain = Some(chain.clone()),
        }
        // The facts of the leaf the target reads without parsing it: the certificate was
        // issued to, and by, the name the client minted it under.
        for (name, expected) in [
            ("x-amzn-mtls-clientcert-subject", format!("CN={CLIENT_NAME}")),
            ("x-amzn-mtls-clientcert-issuer", format!("CN={CLIENT_NAME}")),
        ] {
            assert_eq!(
                reported.get(name).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {:?}: expected {name} to carry {expected:?}, got {reported:?}",
                row.case
            );
        }
        let validity = reported
            .get("x-amzn-mtls-clientcert-validity")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {:?}: no validity in {reported:?}", row.case));
        assert!(
            validity.starts_with("NotBefore=") && validity.contains(";NotAfter="),
            "case {:?}: expected the leaf's two instants, got {validity:?}",
            row.case
        );
        let serial = reported
            .get("x-amzn-mtls-clientcert-serial-number")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {:?}: no serial number in {reported:?}", row.case));
        assert!(
            !serial.is_empty() && serial.chars().all(|digit| digit.is_ascii_hexdigit()),
            "case {:?}: expected the leaf's own serial number, got {serial:?}",
            row.case
        );
        assert!(
            reported
                .get("x-amzn-mtls-clientcert-leaf")
                .and_then(Value::as_str)
                .is_some_and(|leaf| leaf.starts_with("-----BEGIN%20CERTIFICATE-----")),
            "case {:?}: expected the leaf of the chain, got {reported:?}",
            row.case
        );
        // A renamed member travels under the name the listener was configured with and under
        // no other.
        if row.chain_header != CHAIN_HEADER {
            assert_eq!(
                reported.get(CHAIN_HEADER),
                None,
                "case {:?}: expected nothing under the published name, got {reported:?}",
                row.case
            );
        }
    }
}
