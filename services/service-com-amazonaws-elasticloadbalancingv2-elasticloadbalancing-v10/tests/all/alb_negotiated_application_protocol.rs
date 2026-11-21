// SPDX-License-Identifier: CC0-1.0

//! The application protocol a TLS-terminating listener negotiates, end to end in one
//! process: a real `rustls` client dials the address the load balancer published, offers a
//! protocol list, and the suite reads what the handshake selected — and then speaks the
//! selected protocol on that very connection.
//!
//! The rule under test: **the protocols a TLS-terminating listener offers are the ones its
//! own configuration names — a TLS listener's `AlpnPolicy` (`HTTP1Only`, `HTTP2Only`,
//! `HTTP2Optional`, `HTTP2Preferred`, `None`), and, for an Application Load Balancer's
//! HTTPS listener, which carries no such member, the load balancer's own
//! `routing.http2.enabled` attribute — and the handshake selects the first of them the
//! client offered too. What the handshake selected is then what the listener SPEAKS on that
//! connection: a connection that negotiated `h2` is answered over HTTP/2, and one that
//! negotiated `http/1.1` or nothing over HTTP/1.1.**
//!
//! Grounded in the model: `CreateListenerInput$AlpnPolicy` is "[TLS listeners] The name of
//! the Application-Layer Protocol Negotiation (ALPN) policy. You can specify one policy
//! name", with that value list; `LoadBalancerAttribute$Key` carries
//! "`routing.http2.enabled` - Indicates whether HTTP/2 is enabled… The default is true".
//!
//! The rows span what the rule generalises over: the whole published `AlpnPolicy` value
//! list, the two states of the attribute, and the protocol lists a client offers (both
//! protocols in either order, one of them alone, and none at all) — including the client
//! whose only protocol the listener does not offer, whose handshake the listener refuses
//! rather than selecting a protocol it does not serve.
//!
//! Nothing is staged in a store: the certificate is minted by `rcgen` and imported through
//! Certificate Manager's own `ImportCertificate`, the load balancers, listeners and
//! attributes are written through this service's own API, and the client is a real TLS
//! client dialling the published address.

use std::sync::Arc;

use http_body_util::BodyExt as _;
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::alb_https_listener_tls::{
    client_config, create_listener, create_load_balancer, dial, emulator, import, make_active,
    mint, SERVED_BODY, EXCHANGE_TIMEOUT,
};
use crate::provisioning_loop::{action, call, p};

/// The two protocol identifiers the policies name, as the wire spells them.
const HTTP_2: &str = "h2";
const HTTP_1_1: &str = "http/1.1";

/// The load balancer attribute that decides whether an Application Load Balancer serves
/// HTTP/2.
const HTTP2_ENABLED: &str = "routing.http2.enabled";

/// A client configuration offering `protocols` in that order, over the client half the
/// HTTPS suite beside this one dials with — so one verifier and one provider serve every
/// suite that dials a published address.
fn client_offering(protocols: &[&str]) -> Arc<rustls::ClientConfig> {
    let mut config = Arc::try_unwrap(client_config())
        .expect("a freshly built client configuration is held once");
    config.alpn_protocols = protocols
        .iter()
        .map(|protocol| protocol.as_bytes().to_vec())
        .collect();
    Arc::new(config)
}

/// Dial the published address offering `protocols`, and answer the connection beside the
/// protocol the handshake selected — or the handshake's own failure, which is what a client
/// offering nothing the listener offers is answered with.
async fn negotiated(
    published: &str,
    server_name: &str,
    protocols: &[&str],
) -> Result<(TlsStream<TcpStream>, Option<String>), String> {
    let conn = dial(published).await;
    let asked_for = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|error| format!("{server_name} is no server name: {error}"))?;
    let connector = tokio_rustls::TlsConnector::from(client_offering(protocols));
    let stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connector.connect(asked_for, conn))
        .await
        .map_err(|_| "the handshake did not complete in time".to_string())?
        .map_err(|error| error.to_string())?;
    let selected = stream
        .get_ref()
        .1
        .alpn_protocol()
        .map(|protocol| String::from_utf8_lossy(protocol).into_owned());
    Ok((stream, selected))
}

/// Send one HTTP/2 request on a connection that negotiated `h2`, and answer its status and
/// body — which is what proves the listener SPEAKS the protocol it selected.
async fn http_2_answer(case: &str, stream: TlsStream<TcpStream>, authority: &str) -> (u16, String) {
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .unwrap_or_else(|error| panic!("case {case}: the HTTP/2 connection must start, got {error}"));
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/"))
        .body(http_body_util::Full::<bytes::Bytes>::new(bytes::Bytes::new()))
        .expect("a well-formed HTTP/2 request");
    let response = tokio::time::timeout(EXCHANGE_TIMEOUT, sender.send_request(request))
        .await
        .unwrap_or_else(|_| panic!("case {case}: the HTTP/2 request was not answered in time"))
        .unwrap_or_else(|error| panic!("case {case}: the HTTP/2 request must be answered, got {error}"));
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case}: the answer's body must be read, got {error}"))
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Send one HTTP/1.1 request on a connection that negotiated anything else, and answer what
/// came back verbatim.
async fn http_1_1_answer(case: &str, mut stream: TlsStream<TcpStream>, authority: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let request = format!("GET / HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case}: the listener did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).into_owned()
}

/// Write one load balancer attribute, which must be accepted.
async fn modify_attribute(router: &MultiServiceRouter, lb_arn: &str, key: &str, value: &str) {
    let mut form = action("ModifyLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Attributes.member.1.Key", key));
    form.push(p("Attributes.member.1.Value", value));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case write {key}={value}: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// Create an internet-facing NETWORK load balancer, whose TLS listener carries an ALPN
/// policy of its own, and answer its ARN and published address.
async fn create_network_load_balancer(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Type", "network"));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup create network load balancer {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("setup create {name}: no LoadBalancerArn in {}", reply.body));
    let dns_name = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("setup create {name}: no DNSName in {}", reply.body));
    (arn, dns_name)
}

/// Create the TCP target group such a listener forwards to, and answer its ARN.
async fn create_tcp_target_group(router: &MultiServiceRouter, name: &str) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("Protocol", "TCP"));
    form.push(p("Port", "443"));
    form.push(p("VpcId", "vpc-12345678"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup create target group {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("setup create {name}: no TargetGroupArn in {}", reply.body))
}

/// Create the TLS listener of a Network Load Balancer, carrying `alpn_policy` when the case
/// names one.
async fn create_tls_listener(
    router: &MultiServiceRouter,
    lb_arn: &str,
    tg_arn: &str,
    certificate_arn: &str,
    alpn_policy: Option<&str>,
) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "TLS"));
    form.push(p("Port", "443"));
    form.push(p("Certificates.member.1.CertificateArn", certificate_arn));
    if let Some(policy) = alpn_policy {
        form.push(p("AlpnPolicy.member.1", policy));
    }
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", tg_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup create TLS listener ({alpn_policy:?}): expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup create TLS listener: no ListenerArn in {}", reply.body))
}

/// Replace the ALPN policy of an existing listener, through the operation that owns it.
async fn modify_alpn_policy(router: &MultiServiceRouter, listener_arn: &str, policy: &str) {
    let mut form = action("ModifyListener");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("AlpnPolicy.member.1", policy));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case set the ALPN policy to {policy}: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// RULE (the Application family): an HTTPS listener offers HTTP/2 ahead of HTTP/1.1 while
/// its load balancer's `routing.http2.enabled` allows it and HTTP/1.1 alone once a caller
/// switches it off — and the listener answers over the protocol the handshake selected.
#[tokio::test]
async fn an_https_listener_negotiates_what_the_http2_attribute_allows() {
    let (router, engine) = emulator();
    let name = "alpn.listener.example.com";
    let leaf = mint(&[name]);
    let certificate = import(&router, "the listener's certificate", &leaf).await;
    let (lb_arn, published) = create_load_balancer(&router, "alpn-alb").await;
    create_listener(&router, &lb_arn, "HTTPS", "443", Some(&certificate), None).await;
    make_active(&engine, &lb_arn);

    // The rows: what the client offers, and what the handshake must select — with the
    // attribute at the value its documentation carries it at, which is HTTP/2 enabled.
    let enabled: &[(&str, &[&str], Option<&str>)] = &[
        (
            "a client offering both, HTTP/2 first",
            &[HTTP_2, HTTP_1_1],
            Some(HTTP_2),
        ),
        (
            "a client offering both, HTTP/1.1 first",
            &[HTTP_1_1, HTTP_2],
            Some(HTTP_2),
        ),
        ("a client offering HTTP/1.1 alone", &[HTTP_1_1], Some(HTTP_1_1)),
        ("a client offering HTTP/2 alone", &[HTTP_2], Some(HTTP_2)),
        ("a client offering no protocol at all", &[], None),
    ];
    for (case, offered, expected) in enabled {
        let (_stream, selected) = negotiated(&published, name, offered)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
        assert_eq!(
            selected.as_deref(),
            *expected,
            "case {case:?} with HTTP/2 enabled: expected {expected:?}, got {selected:?}"
        );
    }

    // And the connection that negotiated HTTP/2 is ANSWERED over HTTP/2, out of the
    // listener's own action.
    let case = "an HTTP/2 request on the connection that negotiated h2";
    let (stream, selected) = negotiated(&published, name, &[HTTP_2, HTTP_1_1])
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
    assert_eq!(
        selected.as_deref(),
        Some(HTTP_2),
        "case {case:?}: expected h2, got {selected:?}"
    );
    let (status, body) = http_2_answer(case, stream, name).await;
    assert_eq!(
        (status, body.as_str()),
        (200, SERVED_BODY),
        "case {case:?}: expected the listener's own answer over HTTP/2, got {status} {body:?}"
    );

    // The attribute switched off: HTTP/1.1 is all that is offered, so a client offering both
    // negotiates it — and one offering only HTTP/2 is refused the handshake rather than
    // selected a protocol the load balancer does not serve.
    modify_attribute(&router, &lb_arn, HTTP2_ENABLED, "false").await;
    let disabled: &[(&str, &[&str], Option<&str>)] = &[
        (
            "a client offering both",
            &[HTTP_2, HTTP_1_1],
            Some(HTTP_1_1),
        ),
        ("a client offering HTTP/1.1 alone", &[HTTP_1_1], Some(HTTP_1_1)),
        ("a client offering no protocol at all", &[], None),
    ];
    for (case, offered, expected) in disabled {
        let (stream, selected) = negotiated(&published, name, offered)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
        assert_eq!(
            selected.as_deref(),
            *expected,
            "case {case:?} with HTTP/2 disabled: expected {expected:?}, got {selected:?}"
        );
        // Each of them is answered over HTTP/1.1, which is what it negotiated.
        let answer = http_1_1_answer(case, stream, name).await;
        assert!(
            answer.starts_with("HTTP/1.1 200") && answer.contains(SERVED_BODY),
            "case {case:?} with HTTP/2 disabled: expected the listener's own HTTP/1.1 answer, \
             got {answer:?}"
        );
    }
    let case = "a client offering only the protocol the listener does not serve";
    let refused = negotiated(&published, name, &[HTTP_2]).await;
    assert!(
        refused.is_err(),
        "case {case:?}: expected the handshake to be refused, got {:?}",
        refused.map(|(_stream, selected)| selected)
    );

    // And switched back on, the same client negotiates HTTP/2 again: the offer is read per
    // handshake, out of the attribute as it stands.
    modify_attribute(&router, &lb_arn, HTTP2_ENABLED, "true").await;
    let case = "the attribute switched back on";
    let (_stream, selected) = negotiated(&published, name, &[HTTP_2, HTTP_1_1])
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
    assert_eq!(
        selected.as_deref(),
        Some(HTTP_2),
        "case {case:?}: expected h2, got {selected:?}"
    );
}

/// RULE (the Network family): a TLS listener offers the protocols its own `AlpnPolicy`
/// names, in the order that policy prefers them, and a listener carrying no policy offers
/// none.
#[tokio::test]
async fn a_tls_listeners_alpn_policy_decides_what_it_negotiates() {
    let (router, engine) = emulator();
    let name = "alpn.nlb.example.com";
    let leaf = mint(&[name]);
    let certificate = import(&router, "the TLS listener's certificate", &leaf).await;
    let (lb_arn, published) = create_network_load_balancer(&router, "alpn-nlb").await;
    let tg_arn = create_tcp_target_group(&router, "alpn-nlb-tg").await;
    // The listener as a create with no policy leaves it: it offers nothing.
    let listener_arn = create_tls_listener(&router, &lb_arn, &tg_arn, &certificate, None).await;
    make_active(&engine, &lb_arn);

    let case = "a TLS listener carrying no ALPN policy";
    let (_stream, selected) = negotiated(&published, name, &[HTTP_2, HTTP_1_1])
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete, got {error}"));
    assert_eq!(
        selected, None,
        "case {case:?}: expected no protocol selected, got {selected:?}"
    );

    // The rows: the whole published value list of `AlpnPolicy`, each read by a client
    // offering both protocols — which is what makes the ORDER each policy prefers
    // observable — and `HTTP2Only` read once more by a client offering only HTTP/1.1, whose
    // handshake it refuses.
    let rows: &[(&str, &[&str], Option<&str>)] = &[
        ("HTTP1Only", &[HTTP_2, HTTP_1_1], Some(HTTP_1_1)),
        ("HTTP2Only", &[HTTP_2, HTTP_1_1], Some(HTTP_2)),
        ("HTTP2Optional", &[HTTP_2, HTTP_1_1], Some(HTTP_1_1)),
        ("HTTP2Preferred", &[HTTP_2, HTTP_1_1], Some(HTTP_2)),
        ("None", &[HTTP_2, HTTP_1_1], None),
    ];
    for (policy, offered, expected) in rows {
        modify_alpn_policy(&router, &listener_arn, policy).await;
        let (_stream, selected) = negotiated(&published, name, offered)
            .await
            .unwrap_or_else(|error| panic!("case {policy:?}: the handshake must complete, got {error}"));
        assert_eq!(
            selected.as_deref(),
            *expected,
            "case {policy:?}: expected {expected:?}, got {selected:?}"
        );
    }

    modify_alpn_policy(&router, &listener_arn, "HTTP2Only").await;
    let case = "HTTP2Only against a client offering only HTTP/1.1";
    let refused = negotiated(&published, name, &[HTTP_1_1]).await;
    assert!(
        refused.is_err(),
        "case {case:?}: expected the handshake to be refused, got {:?}",
        refused.map(|(_stream, selected)| selected)
    );
}
