// SPDX-License-Identifier: CC0-1.0

//! WHICH viewer TLS handshake an edge address completes.
//!
//! RULE: the edge terminates a viewer's TLS under a certificate for the server name that
//! viewer asked for, so a name no certificate of the address covers is refused in the
//! HANDSHAKE — a fatal `handshake_failure` alert and no session — rather than completed and
//! answered with an HTTP status. The certificates at a connection group's routing endpoint are
//! the TENANTS' own: the endpoint is "the routing endpoint (also known as the DNS name) that is
//! assigned to the connection group", the address a tenant's own domain is pointed at, so the
//! names served there are the domains of the tenants that group routes — and a viewer that
//! dials the routing endpoint itself, asking for no name or for any other, never gets a session
//! on it. At a DISTRIBUTION's own address the names covered are the ones that distribution is
//! served on, plus the address itself for a client that asks for no name.
//!
//! The rows span what the rule generalises over, the NAME a viewer asks for. At a routing
//! endpoint: the domains of two tenants the group routes (served), and the routing endpoint
//! dialled directly with no name asked for, a domain no resource holds and a domain another
//! group's tenant holds (each refused in the handshake). At a distribution's own address: a dial
//! that asks for no name (served, under the certificate of the address it dialled) and a name
//! that distribution is not served on (refused).
//!
//! A tenant here claims a name in a CUSTOMER's zone, validated through a
//! `ManagedCertificateRequest`, because that is the only kind of domain a TLS client can ASK
//! for: what this emulator publishes for a resource of its own is the `host:port` of the
//! listener it answers on, and a client dialling an address sends no server name at all.
//!
//! The `http://` half of either address presents no certificate and is asked for no name, so it
//! is unchanged — which the second case here pins.
//!
//! The topology and the connection group are [`crate::published_routing_endpoint`]'s, the tenant
//! create and its certificate request [`crate::distribution_tenant_lifecycle`]'s, the
//! distribution and its address [`crate::distribution_lifecycle`]'s and the clear-text dial
//! [`crate::published_edge_address`]'s, each reused rather than copied.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, edge_listener_authority, emulator, emulator_with_engine, Answer,
    API_VERSION, POLL_INTERVAL, READY_DEADLINE,
};
use crate::distribution_tenant_lifecycle::{
    create_distribution_tenant, encode_label, escape, managed_certificate_request,
};
use crate::published_edge_address::dial_as;
use crate::published_routing_endpoint::{
    created_group, multi_tenant_distribution, viewer_path, Create, ORIGIN_DOCUMENT,
};

/// How long one viewer's dial waits for its answer, and how long its client waits to connect.
///
/// Deliberately far past what either step needs: the address is a listener of this process, on
/// a machine that may be busy with every other test of this workspace. What a case asserts is
/// WHETHER a session was served, never how quickly, so this exists only to end a dial nothing
/// answers.
const DIAL_DEADLINE: Duration = Duration::from_secs(60);

/// What one viewer's `https://` dial of an address observed.
#[derive(Debug)]
enum Dialled {
    /// A session was served, and this is the HTTP exchange that ran on it.
    Served { status: u16, body: String },
    /// No session at all: the dial never became an HTTP exchange. Carries what the client
    /// reported, which is where the TLS alert it received is named.
    NoSession(String),
}

/// Dial `address` with `https://`, asking for the server name `url` carries, as a stock TLS
/// client handed a name does.
///
/// The client is told which address the name resolves to — a name in a customer's zone is
/// resolved by DNS, the one thing a test cannot ask of the host it runs on — and it does not
/// verify this emulator's self-signed certificate, which is what a viewer of a local emulator
/// does. The dial names the LOOPBACK interface and takes only the PORT from `address`: nothing
/// here opens a connection to anywhere else.
///
/// Unlike [`crate::edge_lambda_url_origin::dial_over_tls`], a dial that is served NO session is
/// an outcome rather than a panic: whether a session is served at all is exactly what the rule
/// here is about.
///
/// The request runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while this call waits.
async fn dial_over_tls(case: &str, address: &str, url: String) -> Dialled {
    let port = address
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| panic!("case {case}: the address {address} is no address: {error}"))
        .port();
    let resolved = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    let case = case.to_string();
    let reported = case.clone();
    ::tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .resolve(&host, resolved)
            .timeout(DIAL_DEADLINE)
            .connect_timeout(DIAL_DEADLINE)
            .build()
            .unwrap_or_else(|error| panic!("case {case}: a TLS client must be built: {error}"));
        match client.get(&url).send() {
            Ok(answered) => Dialled::Served {
                status: answered.status().as_u16(),
                body: answered.text().unwrap_or_default(),
            },
            Err(error) => Dialled::NoSession(format!("{error:?}")),
        }
    })
    .await
    .unwrap_or_else(|error| panic!("case {reported}: the dial did not finish: {error}"))
}

/// Whether `report` is a client's account of a handshake the server REFUSED: the fatal
/// `handshake_failure` alert, which is what a TLS server sends when it holds no certificate for
/// the name the client asked for.
///
/// The alert is named `HandshakeFailure` by the client library, with or without a space
/// depending on which of its renderings carries it, so the comparison ignores both case and
/// spacing rather than pinning one wording of one library.
fn refused_in_the_handshake(report: &str) -> bool {
    report
        .to_ascii_lowercase()
        .replace([' ', '_'], "")
        .contains("handshakefailure")
}

/// Create a tenant of `distribution` holding `domain` — a name in a CUSTOMER's zone, so the
/// create asks CloudFront to validate it — routed through `group`.
async fn tenant_claiming(
    router: &MultiServiceRouter,
    distribution: &str,
    name: &str,
    domain: &str,
    group: &str,
) {
    let body = format!(
        "<CreateDistributionTenantRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <DistributionId>{distribution}</DistributionId><Name>{name}</Name>\
         <Domains><member><Domain>{}</Domain></member></Domains>\
         <ConnectionGroupId>{group}</ConnectionGroupId><Enabled>true</Enabled>{}\
         </CreateDistributionTenantRequest>",
        escape(domain),
        managed_certificate_request()
    );
    let created = create_distribution_tenant(router, &body).await;
    assert_eq!(
        created.status, 201,
        "setup {name}: expected the tenant to be created, got {} with {}",
        created.status, created.raw
    );
}

/// Look again until `domain` resolves a tenant — which it does once the certificate the create
/// asked for has been issued, the state the CLOCK owes it — or the deadline passes, which fails
/// the case. The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_routable_domain(router: &MultiServiceRouter, domain: &str) {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let answer = call(
            router,
            "GET",
            &format!(
                "/{API_VERSION}/distribution-tenant?domain={}",
                encode_label(domain)
            ),
            None,
            None,
        )
        .await;
        if answer.status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup {domain}: the domain never came to resolve a tenant, last answer {}",
            answer.raw
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A connection group's routing endpoint completes a viewer's handshake for the domain of a
/// tenant it routes, and refuses it for every other name — the endpoint itself included — so no
/// viewer is ever handed a session for a name the edge presents no certificate for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routing_endpoint_serves_the_tls_of_the_tenant_domains_it_routes_and_no_other_name() {
    let router = emulator();
    let distribution = multi_tenant_distribution(&router, "ref-handshake-serves").await;
    let (routing_group, routed_endpoint, _) = created_group(
        &router,
        &Create {
            case: "the group the tenants are routed through",
            name: "handshake-routing-group",
            members: "",
        },
    )
    .await;
    let (other_group, _other_endpoint, _) = created_group(
        &router,
        &Create {
            case: "another group, routing a tenant of its own",
            name: "handshake-other-group",
            members: "",
        },
    )
    .await;

    // Two tenants of the group under test, each claiming a domain of its own: the address holds
    // a certificate for both, so neither is a single-name special case. The rows differ in the
    // DEPTH and the spelling of the name a viewer asks for.
    let first = "shop.handshake.example";
    let second = "orders.eu.handshake.example";
    tenant_claiming(
        &router,
        &distribution,
        "handshake-first-tenant",
        first,
        &routing_group,
    )
    .await;
    tenant_claiming(
        &router,
        &distribution,
        "handshake-second-tenant",
        second,
        &routing_group,
    )
    .await;
    // A tenant of the OTHER group: its certificate lives at that group's address, not this one.
    let elsewhere = "elsewhere.handshake.example";
    tenant_claiming(
        &router,
        &distribution,
        "handshake-elsewhere-tenant",
        elsewhere,
        &other_group,
    )
    .await;
    for domain in [first, second, elsewhere] {
        awaited_routable_domain(&router, domain).await;
    }

    for (case, name) in [
        ("the first tenant's own domain", first),
        ("the second tenant's own domain", second),
    ] {
        let dialled = dial_over_tls(
            case,
            &routed_endpoint,
            format!("https://{name}{}", viewer_path()),
        )
        .await;
        match dialled {
            Dialled::Served { status, body } => {
                assert_eq!(
                    status, 200,
                    "case {case}: expected the tenant's distribution to be served over TLS, got \
                     {status} with {body:?}"
                );
                assert!(
                    body.contains(ORIGIN_DOCUMENT),
                    "case {case}: expected the origin's own document, got {body:?}"
                );
            }
            Dialled::NoSession(report) => panic!(
                "case {case}: expected the handshake to complete for a domain this group routes, \
                 got no session at all: {report}"
            ),
        }
    }

    for (case, asked_for) in [
        (
            "the routing endpoint itself, asking for no name",
            format!("https://{routed_endpoint}{}", viewer_path()),
        ),
        (
            "a domain no resource holds",
            format!("https://absent.handshake.example{}", viewer_path()),
        ),
        (
            "a domain another group's tenant holds",
            format!("https://{elsewhere}{}", viewer_path()),
        ),
    ] {
        let dialled = dial_over_tls(case, &routed_endpoint, asked_for).await;
        match dialled {
            Dialled::NoSession(report) => assert!(
                refused_in_the_handshake(&report),
                "case {case}: expected the fatal handshake failure a server with no certificate \
                 for the name sends, got {report}"
            ),
            Dialled::Served { status, body } => panic!(
                "case {case}: expected the TLS handshake to fail, but a session was served with \
                 status {status} and body {body:?}"
            ),
        }
    }
}

/// The CLEAR-TEXT half of that one address is unchanged: no certificate is presented and no
/// server name is asked for there, so there is no handshake to refuse — the tenant's domain is
/// served as before, and the routing endpoint addressed in the clear is still answered over
/// HTTP by nothing of a distribution.
#[tokio::test]
async fn the_clear_text_half_of_a_routing_endpoint_still_answers_over_http() {
    let router = emulator();
    let distribution = multi_tenant_distribution(&router, "ref-handshake-clear").await;
    let (routing_group, routed_endpoint, _) = created_group(
        &router,
        &Create {
            case: "the group the tenant is routed through",
            name: "handshake-clear-group",
            members: "",
        },
    )
    .await;
    let domain = "clear.handshake.example";
    tenant_claiming(
        &router,
        &distribution,
        "handshake-clear-tenant",
        domain,
        &routing_group,
    )
    .await;
    awaited_routable_domain(&router, domain).await;

    let served = dial_as(&routed_endpoint, "GET", &viewer_path(), domain).await;
    assert!(
        served.starts_with("HTTP/1.1 200"),
        "case a clear-text viewer of the tenant's domain: expected the tenant's distribution to \
         be served, got {served:?}"
    );
    assert!(
        served.contains(ORIGIN_DOCUMENT),
        "case a clear-text viewer of the tenant's domain: expected the origin's own document, got \
         {served:?}"
    );

    // The routing endpoint addressed in the clear is still answered by the address itself: no
    // certificate is presented and no name is asked for on that half, so there is no handshake
    // to refuse and the answer is the one this address gives a host it routes nowhere.
    let unrouted = dial_as(&routed_endpoint, "GET", &viewer_path(), &routed_endpoint).await;
    assert!(
        unrouted.starts_with("HTTP/1.1 502"),
        "case a clear-text viewer of the routing endpoint: expected the answer this address \
         gives a host it routes nowhere, got {unrouted:?}"
    );
    assert!(
        !unrouted.contains(ORIGIN_DOCUMENT),
        "case a clear-text viewer of the routing endpoint: expected no distribution to be served \
         for a host this address routes nowhere, got {unrouted:?}"
    );
}

/// A DISTRIBUTION's own address completes the handshake for a viewer that asks for no name — it
/// dialled the address, which this emulator's own certificate covers — and refuses a name that
/// distribution is not served on, which is what a viewer whose own domain was never named among
/// the distribution's alternate domain names gets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_distribution_address_serves_the_tls_of_the_names_it_is_served_on() {
    let (router, engine) = emulator_with_engine();
    let case = "a distribution's own published address";
    let created = create_distribution(
        &router,
        &crate::distribution_lifecycle::config(case, "ref-handshake-distribution").xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected the distribution to be created, got {} with {}",
        created.status, created.raw
    );
    let id = member(&created, "Id", case);
    let address = edge_listener_authority(&engine, case, &id);

    // A viewer that asks for NO name: it dials the address the distribution published, which is
    // the address this emulator answers on and its own certificate covers.
    let under_no_name = dial_over_tls(
        case,
        &address,
        format!("https://{address}/{API_VERSION}/distribution"),
    )
    .await;
    assert!(
        matches!(under_no_name, Dialled::Served { .. }),
        "case {case}: expected the handshake to complete for a viewer that asked for no name, \
         got {under_no_name:?}"
    );

    // A name this distribution is not served on: no certificate here covers it.
    let elsewhere = dial_over_tls(
        case,
        &address,
        format!("https://unnamed.handshake.example/{API_VERSION}/distribution"),
    )
    .await;
    match elsewhere {
        Dialled::NoSession(report) => assert!(
            refused_in_the_handshake(&report),
            "case {case}: expected the fatal handshake failure a server with no certificate for \
             the name sends, got {report}"
        ),
        Dialled::Served { status, body } => panic!(
            "case {case}: expected the TLS handshake to fail for a name this distribution is not \
             served on, but a session was served with status {status} and body {body:?}"
        ),
    }
}

/// The text of `member` in `answer`, or a panic naming the case.
fn member(answer: &Answer, member: &str, case: &str) -> String {
    answer
        .root()
        .find_text(member)
        .unwrap_or_else(|| panic!("case {case}: the answer carries no {member}: {}", answer.raw))
        .to_string()
}
