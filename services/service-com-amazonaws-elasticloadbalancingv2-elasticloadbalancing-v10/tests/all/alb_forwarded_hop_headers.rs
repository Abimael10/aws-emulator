// SPDX-License-Identifier: CC0-1.0

//! The HOP a request took, as the target behind an Application Load Balancer is told about it:
//! the `X-Forwarded-Proto` and `X-Forwarded-Port` a `lambda` target receives, and their
//! independence from the attribute that decides the `X-Forwarded-For` travelling beside them.
//!
//! The rule under test: **a request an Application Load Balancer forwards to a target reports
//! the LISTENER it arrived on — `X-Forwarded-Proto` is that listener's protocol lower-cased
//! and `X-Forwarded-Port` is that listener's port as a decimal string, each REPLACING whatever
//! the client claimed under the same name — whatever the load balancer's
//! `routing.http.xff_header_processing.mode` holds; the attribute governs `X-Forwarded-For`
//! alone, which the same request carries as the mode gives it.**
//!
//! `LoadBalancerAttribute$Key` documents the attribute and the three values its domain admits:
//! "`routing.http.xff_header_processing.mode` - Enables you to modify, preserve, or remove the
//! X-Forwarded-For header in the HTTP request before the Application Load Balancer sends the
//! request to the target. The possible values are append, preserve, and remove. The default is
//! append", and per value: `append` "adds the client IP address (of the last hop) to the
//! X-Forwarded-For header in the HTTP request before it sends it to targets", `preserve`
//! "preserves the X-Forwarded-For header in the HTTP request, and sends it to targets without
//! any change", `remove` "removes the X-Forwarded-For header in the HTTP request before it
//! sends it to targets". Nothing in it is about the other two headers, which report the hop
//! itself — the listener the request reached, which is this service's own record of it.
//!
//! The rows span what the rule generalises over: each LISTENER of one load balancer — both
//! protocols an Application Load Balancer's listeners carry (`HTTP` and `HTTPS`, the two
//! `ProtocolEnum` values `CreateListener` admits for that family) and, for each, the port its
//! scheme implies and one other port, so neither the protocol nor the port of a row can be a
//! constant — against each of the three modes, driven with the client's own `X-Forwarded-For`
//! absent, carrying one address and carrying a list, and with the client CLAIMING hop headers
//! of its own (a protocol it did not arrive over and a port no listener of the load balancer
//! answers on) on half the rows.
//!
//! Every mode a row is driven against is written through `ModifyLoadBalancerAttributes` and
//! read back through `DescribeLoadBalancerAttributes` first, so no row rests on a value a test
//! believes it wrote. What the target was handed is reported by the TARGET: a Lambda function
//! created through Lambda's own API, run by the Lambda emulator mounted on this suite's
//! engine, which echoes the headers of the event it was handed — the fixture
//! `crate::alb_drop_invalid_header_fields` owns.
//!
//! The FORMS the client's own `X-Forwarded-For` takes — including the header sent twice, which
//! is one list — are the table of `crate::alb_desync_mitigation_and_forwarded_for`, which owns
//! that half of the rule; the value asserted here is what makes the hop headers' independence
//! from the mode visible (a mode that changed them, or that stopped governing the header it
//! does govern, fails a row), and it is computed from the request rather than written out. The
//! write-and-read-back pair below is this suite's own because that suite's is bound to its own
//! deployment, and the ONE reading of the address a request comes from is imported from it.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::alb_desync_mitigation_and_forwarded_for::CONNECTION_ADDRESS;
use crate::alb_drop_invalid_header_fields::{emulator, lambda, package, HANDLER, ROLE, RUNTIME};
use crate::provisioning_loop::{action, attribute_sequence, call, p, REGION};

/// The attribute whose three values must change nothing about the two hop headers.
const XFF_ATTRIBUTE: &str = "routing.http.xff_header_processing.mode";

/// The header the attribute governs, and the two that report the hop itself.
const FORWARDED_FOR: &str = "x-forwarded-for";
const FORWARDED_PROTO: &str = "x-forwarded-proto";
const FORWARDED_PORT: &str = "x-forwarded-port";

/// A header of the client's that no mode and no hop reporting touches, carried by every row.
const KEPT_HEADER: &str = "x-echo-kept";
const KEPT_VALUE: &str = "untouched";

/// The endpoint of THIS API, which is the host a leg a load balancer's own socket forwarded
/// carries, with the PORT of the listener the connection reached on it — the addressing that
/// names one listener of a load balancer that has several
/// (`crate::authentication_callback_address` drives the same one).
const SERVICE_ENDPOINT: &str = "elasticloadbalancingv2.amazonaws.com";

/// A port no listener below answers on, which is what a row's claiming client reports itself
/// as having dialled.
const CLAIMED_PORT: &str = "65535";

/// The function the load balancer forwards to, and the `lambda` target group holding it.
const FUNCTION: &str = "alb-hop-echo";
const LOAD_BALANCER: &str = "hop-headers-alb";
const TARGET_GROUP: &str = "hop-headers-tg";

/// Everything the rows are driven against: the router every call goes through, the load
/// balancer's ARN, and the address it published.
struct Deployed {
    router: MultiServiceRouter,
    lb_arn: String,
    dns_name: String,
}

/// One listener of that load balancer, and how a request reaches it.
struct Listener {
    case: &'static str,
    /// The `Protocol` the listener was created with, as `CreateListener` took it.
    protocol: &'static str,
    /// The `Port` it was created with.
    port: u16,
    /// Whether a row reaches it by the address the load balancer PUBLISHES — the addressing a
    /// client handed `LoadBalancer$DNSName` uses, which reaches the lowest-numbered listener.
    /// Every other listener is named by the port of its own leg.
    by_published_address: bool,
}

impl Listener {
    /// The host and the request target one row addresses this listener by.
    fn addressing(&self, published: &str) -> (String, String) {
        if self.by_published_address {
            return (published.to_string(), "/hop".to_string());
        }
        (
            format!("{SERVICE_ENDPOINT}:{}", self.port),
            format!("/_aws/elasticloadbalancing/{published}/hop"),
        )
    }

    /// The protocol a CLIENT of this listener claims to have arrived over, which is never the
    /// one it did: the other protocol of the pair.
    fn claimed_protocol(&self) -> &'static str {
        if self.protocol.eq_ignore_ascii_case("HTTP") {
            "https"
        } else {
            "http"
        }
    }
}

/// The listeners of the one load balancer, spanning the protocol and the port the hop headers
/// are read off: both protocols, each on the port its scheme implies and on another one.
fn listeners() -> Vec<Listener> {
    vec![
        Listener {
            case: "an HTTP listener on the port http implies",
            protocol: "HTTP",
            port: 80,
            by_published_address: true,
        },
        Listener {
            case: "an HTTP listener on another port",
            protocol: "HTTP",
            port: 8080,
            by_published_address: false,
        },
        Listener {
            case: "an HTTPS listener on the port https implies",
            protocol: "HTTPS",
            port: 443,
            by_published_address: false,
        },
        Listener {
            case: "an HTTPS listener on another port",
            protocol: "HTTPS",
            port: 8443,
            by_published_address: false,
        },
    ]
}

/// One request a row makes: the mode the load balancer carries, the `X-Forwarded-For` fields
/// the client sends, and whether it claims hop headers of its own.
struct Row {
    case: &'static str,
    mode: &'static str,
    sent: &'static [&'static str],
    claims_the_hop: bool,
}

/// The rows, spanning each mode against a client that supplied one address, a list, and
/// nothing at all, half of them claiming the hop themselves.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "append onto one supplied address",
            mode: "append",
            sent: &["203.0.113.7"],
            claims_the_hop: false,
        },
        Row {
            case: "append where the client supplied none",
            mode: "append",
            sent: &[],
            claims_the_hop: true,
        },
        Row {
            case: "preserve a supplied list",
            mode: "preserve",
            sent: &["203.0.113.7, 198.51.100.4"],
            claims_the_hop: true,
        },
        Row {
            case: "remove a supplied address",
            mode: "remove",
            sent: &["203.0.113.7"],
            claims_the_hop: false,
        },
    ]
}

/// What the target must be handed as `X-Forwarded-For` for one row: the mode's own doing,
/// computed from the list the client supplied and the address the request came from rather
/// than written out, so no row can be read as pinning one recorded value.
fn expected_forwarded_for(row: &Row) -> Option<String> {
    let supplied: Vec<&str> = row
        .sent
        .iter()
        .copied()
        .filter(|value| !value.trim().is_empty())
        .collect();
    match row.mode {
        "append" if supplied.is_empty() => Some(CONNECTION_ADDRESS.to_string()),
        "append" => Some(format!("{}, {CONNECTION_ADDRESS}", supplied.join(", "))),
        "preserve" => (!supplied.is_empty()).then(|| supplied.join(", ")),
        _ => None,
    }
}

/// A load balancer whose every listener forwards to one `lambda` target group holding a
/// function that echoes the headers of the event it is handed — every resource through the
/// API that owns it.
async fn deployed() -> Deployed {
    let (router, engine) = emulator();

    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
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
    // The activation span Lambda owes the function, placed rather than slept for.
    {
        let state = engine
            .state::<service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for record in store.functions.values_mut() {
            record.active_due = past;
        }
    }

    let (lb_arn, dns_name) = create_load_balancer(&router, LOAD_BALANCER).await;

    let mut form = action("CreateTargetGroup");
    form.push(p("Name", TARGET_GROUP));
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

    // The certificate the HTTPS listeners carry. The service that holds certificates is not
    // mounted here and a certificate its owner does not answer about is not refused, so a
    // well-formed ARN is what such a listener names; the certificate actually PRESENTED on a
    // handshake is `crate::alb_https_listener_tls`.
    let certificate =
        format!("arn:aws:acm:{REGION}:000000000000:certificate/00000000-0000-0000-0000-000000000000");
    for listener in listeners() {
        let mut form = action("CreateListener");
        form.push(p("LoadBalancerArn", &lb_arn));
        form.push(p("Protocol", listener.protocol));
        form.push(p("Port", &listener.port.to_string()));
        if listener.protocol.eq_ignore_ascii_case("HTTPS") {
            form.push(p("Certificates.member.1.CertificateArn", &certificate));
        }
        form.push(p("DefaultActions.member.1.Type", "forward"));
        form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup CreateListener {}: expected 200, got {} body {}",
            listener.case, reply.status, reply.body
        );
    }

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    Deployed {
        router,
        lb_arn,
        dns_name,
    }
}

/// What `DescribeLoadBalancerAttributes` reports the attribute at.
async fn described(deployed: &Deployed, case: &str) -> String {
    let mut form = action("DescribeLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", &deployed.lb_arn));
    let reply = call(&deployed.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected DescribeLoadBalancerAttributes to answer 200, got {} body {}",
        reply.status, reply.body
    );
    attribute_sequence(&reply.body)
        .into_iter()
        .find(|(key, _)| key == XFF_ATTRIBUTE)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("case {case:?}: no {XFF_ATTRIBUTE} in {}", reply.body))
}

/// Write one mode, which must be accepted, and read it back, so every row is driven against a
/// value the control plane REPORTS rather than one this suite believes it wrote.
async fn written(deployed: &Deployed, case: &str, mode: &str) {
    let mut form = action("ModifyLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", &deployed.lb_arn));
    form.push(p("Attributes.member.1.Key", XFF_ATTRIBUTE));
    form.push(p("Attributes.member.1.Value", mode));
    let reply = call(&deployed.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected ModifyLoadBalancerAttributes to answer 200, got {} body {}",
        reply.status, reply.body
    );
    let reported = described(deployed, case).await;
    assert_eq!(
        reported, mode,
        "case {case:?}: expected {XFF_ATTRIBUTE} to be reported at {mode:?}, got {reported:?}"
    );
}

/// The headers the TARGET was handed for one row on one listener, as the function echoed them.
async fn seen_by_the_target(
    deployed: &Deployed,
    case: &str,
    listener: &Listener,
    row: &Row,
) -> serde_json::Map<String, Value> {
    let mut headers: Vec<(&str, &str)> = row
        .sent
        .iter()
        .map(|value| (FORWARDED_FOR, *value))
        .collect();
    headers.push((KEPT_HEADER, KEPT_VALUE));
    if row.claims_the_hop {
        headers.push((FORWARDED_PROTO, listener.claimed_protocol()));
        headers.push((FORWARDED_PORT, CLAIMED_PORT));
    }
    let (host, target) = listener.addressing(&deployed.dns_name);
    let answered = send(&deployed.router, "GET", &host, &target, &headers).await;
    assert_eq!(
        answered.status, 200,
        "case {case}: expected the target's HTTP 200, got {} with body {:?}",
        answered.status, answered.body
    );
    let echoed: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "case {case}: the target's echo is not JSON ({error}) in {:?}",
            answered.body
        )
    });
    match echoed {
        Value::Object(headers) => headers,
        other => panic!("case {case}: the target's echo is no object, got {other}"),
    }
}

/// RULE: the hop headers a target receives are the protocol and the port of the LISTENER the
/// request arrived on, in every mode of the attribute that decides the `X-Forwarded-For`
/// beside them — and that header is the one the mode gives it.
#[tokio::test]
async fn a_target_is_told_the_listener_it_was_reached_through_in_every_forwarded_for_mode() {
    let deployed = deployed().await;
    let listeners = listeners();

    // The listener a row reaches by the PUBLISHED address is the lowest-numbered one, because
    // that is the listener a request naming no listener port of its own reaches: asserted here
    // rather than assumed, so a listener added below it fails loudly instead of answering a row
    // written for another.
    let lowest = listeners
        .iter()
        .map(|listener| listener.port)
        .min()
        .expect("the load balancer carries listeners");
    for listener in &listeners {
        assert_eq!(
            listener.by_published_address,
            listener.port == lowest,
            "case {:?}: the published address reaches the lowest-numbered listener ({lowest}), \
             so only that one may be addressed by it",
            listener.case
        );
    }

    for row in rows() {
        written(&deployed, row.case, row.mode).await;
        let expected_for = expected_forwarded_for(&row);
        for listener in &listeners {
            let case = format!("{:?} through {:?}", row.case, listener.case);
            let seen = seen_by_the_target(&deployed, &case, listener, &row).await;

            // The hop: the listener's own protocol, lower-cased, and its own port as a
            // decimal string — never the protocol or the port the client claimed.
            let proto = seen.get(FORWARDED_PROTO).and_then(Value::as_str);
            let expected_proto = listener.protocol.to_ascii_lowercase();
            assert_eq!(
                proto,
                Some(expected_proto.as_str()),
                "case {case}: expected {FORWARDED_PROTO} at {expected_proto:?}, got {proto:?} \
                 in {seen:?}"
            );
            let port = seen.get(FORWARDED_PORT).and_then(Value::as_str);
            let expected_port = listener.port.to_string();
            assert_eq!(
                port,
                Some(expected_port.as_str()),
                "case {case}: expected {FORWARDED_PORT} at {expected_port:?}, got {port:?} in \
                 {seen:?}"
            );

            // The header the mode governs, which is the only one it changes.
            let delivered = seen.get(FORWARDED_FOR).and_then(Value::as_str);
            assert_eq!(
                delivered,
                expected_for.as_deref(),
                "case {case}: expected {FORWARDED_FOR} at {expected_for:?}, got {delivered:?} \
                 in {seen:?}"
            );

            // Every other header of the same request reaches the target untouched.
            let kept = seen.get(KEPT_HEADER).and_then(Value::as_str);
            assert_eq!(
                kept,
                Some(KEPT_VALUE),
                "case {case}: expected {KEPT_HEADER} at {KEPT_VALUE:?}, got {kept:?} in {seen:?}"
            );
        }
    }
}
