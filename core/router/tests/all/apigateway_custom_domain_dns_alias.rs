// SPDX-License-Identifier: CC0-1.0

//! A Route 53 ALIAS record set pointed at a REGIONAL custom domain name resolves to the
//! ADDRESS that custom domain's front door answers on.
//!
//! The rule under test: **`DomainName$regionalDomainName` is "the domain name associated
//! with the regional endpoint for this custom domain name. You set up this association by
//! adding a DNS record that points the custom domain name to this regional domain name",
//! and `RecordDataEntry` documents the answer for an alias to an Amazon Web Services
//! resource as "an IP address or a domain name for the … resource" — so an `A` alias
//! targeting what API Gateway published is answered by Route 53 with the IPv4 ADDRESS a
//! client then reaches that custom domain at, and a request arriving there for a Host no
//! base path mapping answers is refused `403`.** Two crates have to agree for that to hold
//! (API Gateway publishes and serves the address, Route 53 resolves the alias), so the suite
//! lives here rather than in either of them.
//!
//! What this suite does NOT assert is the PORT: an `A` record carries an address and no
//! port, so a client that dials the resolved address on the port `https` implies reaches
//! this emulator only where the core assigned the front door that port — `Listeners` binds
//! within the operator's own range (`EMULATOR_LISTENER_PORT_RANGE`, OS-assigned when unset)
//! and takes no requested port. The address Route 53 answers with is therefore read here
//! together with the port the control plane published, which is the whole of what a client
//! on this machine needs to reach the front door.
//!
//! The rows differ along what the rule generalises over: how deep the custom domain's name
//! is under the zone it is aliased from (a label under the apex, two labels, the apex
//! itself), and the `securityPolicy` the record carries. Every expected value is computed
//! from what API Gateway published, never from the answer being checked.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// The XML element readers and the query-component encoder this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{element, elements, form_encode, opt_element};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// How often, and for how long, the suite asks whether a clock-owed transition has come due
/// (`DOMAIN_NAME_CREATE_SPAN`, the change batch's own propagation). A generous budget: a
/// loaded machine is slower than the spans these services choose without the rule being any
/// different.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// How long one dial and one exchange on the published address may take. All of it is in
/// this process; a case that cannot finish inside it has found a defect, not a slow machine.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The `domainNameStatus` a custom domain name is usable in.
const AVAILABLE: &str = "AVAILABLE";

/// The `ChangeInfo$Status` a change batch reaches once every Route 53 name server has it.
const INSYNC: &str = "INSYNC";

/// The response code of a DNS query the zone answers.
const NOERROR: &str = "NOERROR";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// Call one Amazon API Gateway operation over its restJson1 wire.
async fn apigateway(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one Amazon Route 53 operation over its restXml wire.
async fn route53(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("route53.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// A string member of an answered document, or a panic quoting the whole document.
fn member(document: &Value, name: &str, case: &str) -> String {
    document
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: expected a {name} in {document}"))
        .to_string()
}

/// One row: the custom domain name to create, the zone to alias it from, and the security
/// policy the record carries.
struct Row {
    case: &'static str,
    /// The hosted zone the alias record set lives in.
    zone: &'static str,
    /// The custom domain name, which is also the alias record set's name.
    domain_name: &'static str,
    /// The `securityPolicy` the custom domain is created with.
    security_policy: &'static str,
}

#[tokio::test]
async fn an_alias_to_a_regional_custom_domain_resolves_to_the_address_it_is_served_on() {
    let rows: &[Row] = &[
        Row {
            case: "a label under the zone's apex",
            zone: "front-door-dns.example.com",
            domain_name: "shop.front-door-dns.example.com",
            security_policy: "TLS_1_2",
        },
        Row {
            case: "two labels under the apex",
            zone: "deep-front-door-dns.example.com",
            domain_name: "api.eu.deep-front-door-dns.example.com",
            security_policy: "TLS_1_0",
        },
        Row {
            case: "the zone's own apex",
            zone: "apex-front-door-dns.example.com",
            domain_name: "apex-front-door-dns.example.com",
            security_policy: "TLS_1_2",
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();

        // 1. The custom domain name, created through API Gateway's own API, and read back
        //    until the status its create owes has come due.
        let (status, created) = apigateway(
            &router,
            "POST",
            "/domainnames",
            Some(json!({
                "domainName": row.domain_name,
                "regionalCertificateArn": format!(
                    "arn:aws:acm:{REGION}:123456789012:certificate/\
                     11111111-2222-3333-4444-555555555555"
                ),
                "endpointConfiguration": {"types": ["REGIONAL"]},
                "securityPolicy": row.security_policy,
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateDomainName, got {status} with {created}"
        );

        let mut record = created;
        for _ in 0..POLLS {
            if record.get("domainNameStatus").and_then(Value::as_str) == Some(AVAILABLE) {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
            let (status, read) = apigateway(
                &router,
                "GET",
                &format!("/domainnames/{}", row.domain_name),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from GetDomainName, got {status} with {read}"
            );
            record = read;
        }
        let reported_status = member(&record, "domainNameStatus", case);
        assert_eq!(
            reported_status, AVAILABLE,
            "case {case:?}: expected the domain to reach {AVAILABLE}, got \
             {reported_status:?} in {record}"
        );
        let published = member(&record, "regionalDomainName", case);
        let hosted_zone_id = member(&record, "regionalHostedZoneId", case);
        assert!(
            !hosted_zone_id.is_empty(),
            "case {case:?}: expected a regional hosted zone id to alias with, got \
             {hosted_zone_id:?}"
        );
        // The address a client dials: the `host:port` the front door answers on.
        let authority: std::net::SocketAddr = published.parse().unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected regionalDomainName to be an address a client can \
                 dial, got {published:?}: {error}"
            )
        });

        // 2. The zone, and the ALIAS record set that points the custom domain at what API
        //    Gateway published — exactly the record AWS documents for this association.
        let (status, zone) = route53(
            &router,
            "POST",
            "/2013-04-01/hostedzone",
            &format!(
                "<CreateHostedZoneRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><Name>{}</Name>\
                 <CallerReference>{}</CallerReference></CreateHostedZoneRequest>",
                row.zone, row.domain_name
            ),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateHostedZone, got {status} with {zone}"
        );
        let zone_id = element(&zone, "Id")
            .trim_start_matches("/hostedzone/")
            .to_string();

        let (status, changed) = route53(
            &router,
            "POST",
            &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
            &format!(
                "<ChangeResourceRecordSetsRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><ChangeBatch>\
                 <Changes><Change><Action>CREATE</Action><ResourceRecordSet>\
                 <Name>{}</Name><Type>A</Type><AliasTarget>\
                 <HostedZoneId>{hosted_zone_id}</HostedZoneId><DNSName>{published}</DNSName>\
                 <EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget>\
                 </ResourceRecordSet></Change></Changes></ChangeBatch>\
                 </ChangeResourceRecordSetsRequest>",
                row.domain_name
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ChangeResourceRecordSets, got {status} with \
             {changed}"
        );
        let change_id = element(&changed, "Id")
            .trim_start_matches("/change/")
            .to_string();
        let mut change_status = element(&changed, "Status");
        for _ in 0..POLLS {
            if change_status == INSYNC {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
            let (status, read) =
                route53(&router, "GET", &format!("/2013-04-01/change/{change_id}"), "").await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from GetChange, got {status} with {read}"
            );
            change_status = element(&read, "Status");
        }
        assert_eq!(
            change_status, INSYNC,
            "case {case:?}: expected the change to reach {INSYNC}, got {change_status:?}"
        );

        // 3. What a resolver gets for the custom domain: the ADDRESS the front door answers
        //    on, and nothing that is not record data of an `A` record set.
        let (status, answered) = route53(
            &router,
            "GET",
            &format!(
                "/2013-04-01/testdnsanswer?hostedzoneid={zone_id}&recordname={}&recordtype=A",
                form_encode(row.domain_name)
            ),
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from TestDNSAnswer, got {status} with {answered}"
        );
        assert_eq!(
            opt_element(&answered, "ResponseCode").as_deref(),
            Some(NOERROR),
            "case {case:?}: expected {NOERROR}, got {answered}"
        );
        let resolved = elements(&answered, "RecordDataEntry");
        assert_eq!(
            resolved,
            vec![authority.ip().to_string()],
            "case {case:?}: expected the address the front door answers on \
             ({}), got {resolved:?}",
            authority.ip()
        );
        let address: Ipv4Addr = resolved[0].parse().unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected an IPv4 address in the A answer, got {:?}: {error}",
                resolved[0]
            )
        });

        // 4. And the address really is the custom domain's front door: a request for this
        //    Host arrives there and is refused `403`, because the domain has no base path
        //    mapping. (The port comes from the control plane, which is where a client on
        //    this machine reads it: DNS carries the address alone.)
        //
        //    The core binds its listeners on the loopback interface by default
        //    (`EMULATOR_BIND_ADDRESS`), so the address Route 53 answered with is the one
        //    [`exchange`] dials by its loopback literal; asserting that equality here is
        //    what keeps the dial below about the RESOLVED value.
        assert_eq!(
            address,
            Ipv4Addr::LOCALHOST,
            "case {case:?}: expected the resolved address to be the loopback interface the \
             core binds its listeners on, got {address}"
        );
        let answer = exchange(case, authority.port(), row.domain_name).await;
        assert!(
            answer.starts_with("HTTP/1.1 403"),
            "case {case:?}: expected the front door at {address} to refuse a Host with no \
             base path mapping with 403, got {answer:?}"
        );
    }
}

/// Dial this emulator's own `port` on the loopback interface the core binds its listeners on
/// and send `GET /` for `host`, answering the status line and headers the address replied
/// with.
async fn exchange(case: &str, port: u16, host: &str) -> String {
    let connecting = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    let mut stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connecting)
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: port {port} did not accept a connection"))
        .unwrap_or_else(|error| {
            panic!("case {case:?}: port {port} refused a connection: {error}")
        });
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut raw = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: port {port} did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&raw).to_string()
}
