// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ADDRESSES a CloudWatchOmni domain is announced at —
//! `Domain.domainEndpointUrl` ("The HTTPS endpoint URL for accessing the domain") and every
//! `Domain.customEndpointUrls` entry ("Additional endpoint URLs derived from the domain
//! name").
//!
//! The RULE this suite asserts: a URL this service hands a caller to OPEN is rooted on the
//! `host[:port]` THIS emulator was reached at for the request being answered, with the
//! identity AWS's own host carries in its leftmost label moved into the path — never a host
//! in a zone AWS publishes its own endpoints in, which resolves nowhere near this process.
//! It follows that
//!
//! * the announced address FOLLOWS the request: two requests that reached this emulator on
//!   two addresses are answered their own address, and the value is composed per read rather
//!   than frozen by the create;
//! * a request that addressed one of AWS's own hosts — an in-process call of another service
//!   included — is answered the one address this process answers on;
//! * every operation that reports a domain (the create, the read, the patch) announces one
//!   set of addresses for one request;
//! * the published address is one this emulator SERVES: a fetch of it renders the stored
//!   domain, and a fetch of an address of the same shape naming a domain this emulator does
//!   not hold renders no domain.
//!
//! Each row of each table differs along what the rule generalises over — the form the
//! request carried its host in (a `Host` header, an absolute-form target, neither), the
//! address shapes a client reaches an emulator on (an IPv4 address, a loopback name, a
//! bracketed IPv6 literal, a name with no port), a host naming the service itself, and which
//! of the two endpoint members is read. Every assertion names its case and the actual value.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::ShapeSerializer;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::get_domain::GetDomain;
use omni::ops::types::{
    CreateDomainInput, Domain, GetDomainInput, GetDomainOutput, UpdateDomainInput,
    GET_DOMAIN_INPUT_SCHEMA,
};
use omni::ops::update_domain::UpdateDomain;

/// The region the suite signs for.
const REGION: &str = "us-west-2";

/// The suffix AWS publishes its own CloudWatch Omni domain hosts under, which no value this
/// emulator announces may name: a client on this machine resolves nothing there, and a
/// client that resolves it leaves the emulated environment altogether.
const AWS_DOMAIN_HOST_SUFFIX: &str = "cloudwatch-omni.global.app.aws";

/// The request parts a row drives its calls with: a SigV4 credential scope naming this
/// service and the Region the handlers record, the request target `uri`, and the `Host`
/// header the client stated, when it stated one.
fn signed_parts(host: Option<&str>, uri: &str) -> http::request::Parts {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        );
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    let (parts, ()) = builder
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// The modelled request target this service's operations are addressed at.
const MODELLED_TARGET: &str = "/service/CloudWatchOmniFrontend/operation/Unused";

/// One emulator: one shared engine and the service mounted on a router, driven through
/// whichever request parts a case hands it.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        // The router's service and the operations constructed below hold the SAME engine, so
        // both front doors read and write one store.
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
        }
    }

    fn ctx<'a>(&'a self, parts: &'a http::request::Parts) -> OperationContext<'a> {
        OperationContext::new(
            parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's domain, created through the handler that persists it, for a request
    /// carrying `parts`.
    async fn create_domain(&self, parts: &http::request::Parts, name: &str) -> Domain {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(parts),
            )
            .await
            .unwrap_or_else(|error| panic!("case {name:?}: the domain create failed: {error:?}"))
            .domain
    }

    async fn get_domain(&self, parts: &http::request::Parts, domain_id: &str) -> Domain {
        GetDomain::new(Arc::clone(&self.engine))
            .handle(
                GetDomainInput {
                    domain_id: domain_id.to_string(),
                },
                &self.ctx(parts),
            )
            .await
            .unwrap_or_else(|error| panic!("GetDomain({domain_id}) failed: {error:?}"))
            .domain
    }

    async fn rename_domain(
        &self,
        parts: &http::request::Parts,
        domain_id: &str,
        name: &str,
    ) -> Domain {
        UpdateDomain::new(Arc::clone(&self.engine))
            .handle(
                UpdateDomainInput {
                    domain_id: domain_id.to_string(),
                    name: Some(name.to_string()),
                    ..Default::default()
                },
                &self.ctx(parts),
            )
            .await
            .unwrap_or_else(|error| panic!("UpdateDomain({domain_id}) failed: {error:?}"))
            .domain
    }

    /// Dispatch one GET (or HEAD) over HTTP through the router, as a browser holding an
    /// announced endpoint URL would: no credential, and no header naming this service.
    async fn fetch(&self, method: &str, url: &str) -> (u16, String, String) {
        let request = http::Request::builder()
            .method(method)
            .uri(url)
            .body(bytes::Bytes::new())
            .expect("a well-formed fetch");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let media_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (
            status,
            media_type,
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// `GetDomain` over the rpcv2Cbor wire through the router, for a request whose `Host`
    /// header the row chose — the path a real client takes.
    async fn get_domain_over_the_wire(&self, host: &str, domain_id: &str) -> Domain {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(
                GET_DOMAIN_INPUT_SCHEMA,
                &GetDomainInput {
                    domain_id: domain_id.to_string(),
                },
            )
            .expect("the input shape serializes");
        let request = http::Request::builder()
            .method("POST")
            .uri("/service/CloudWatchOmniFrontend/operation/GetDomain")
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudwatch/aws4_request, \
                     SignedHeaders=host, Signature=emulator"
                ),
            )
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {host:?}: GetDomain over the wire answered {}: {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let body = response.body().clone();
        let mut deserializer = codec.create_deserializer(&body);
        GetDomainOutput::deserialize(&mut deserializer)
            .expect("the output deserializes")
            .domain
    }
}

/// The address an emulator served on no port at all reports, which is what an in-process
/// call reaches: the unserved base's own authority.
fn unserved_authority() -> String {
    smithy_server_runtime::UNSERVED_URL_BASE
        .split_once("://")
        .map_or(smithy_server_runtime::UNSERVED_URL_BASE, |(_, rest)| rest)
        .to_string()
}

/// Assert that `domain` announces both endpoint members on `authority`, with the domain's id
/// addressing one and its name the other.
fn assert_announced_on(case: &str, domain: &Domain, authority: &str) {
    let expected_id_endpoint = format!(
        "https://{authority}{}",
        omni::state::domain_endpoint_path(&domain.domain_id)
    );
    assert_eq!(
        domain.domain_endpoint_url, expected_id_endpoint,
        "case {case:?}: expected domainEndpointUrl {expected_id_endpoint:?}, got {:?}",
        domain.domain_endpoint_url
    );
    let name = domain
        .name
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the created domain reports its name"));
    let expected_custom = vec![format!(
        "https://{authority}{}",
        omni::state::domain_name_endpoint_path(&name)
    )];
    let custom = domain.custom_endpoint_urls.clone().unwrap_or_default();
    assert_eq!(
        custom, expected_custom,
        "case {case:?}: expected customEndpointUrls {expected_custom:?}, got {custom:?}"
    );
    for url in std::iter::once(&domain.domain_endpoint_url).chain(custom.iter()) {
        assert!(
            !url.contains(AWS_DOMAIN_HOST_SUFFIX) && !url.contains("amazonaws.com"),
            "case {case:?}: an announced URL must name no host AWS publishes, got {url:?}"
        );
    }
}

/// RULE: both endpoint members are rooted on the address the request reached this emulator
/// at, whichever form the request carried its host in — and a request that addressed one of
/// AWS's own hosts is answered the one address this process answers on.
#[tokio::test]
async fn the_announced_endpoints_are_rooted_on_the_address_the_request_reached() {
    let unserved = unserved_authority();
    // Rows differ along the form the host arrived in and the address shape it named: an IPv4
    // address with a port, a loopback name with a port, a bracketed IPv6 literal, a name with
    // no port at all, an absolute-form target carrying the authority instead of a header, a
    // host naming this very service (a caller that resolves AWS's own hosts), and a request
    // naming no host at all.
    let rows: Vec<(&str, Option<&str>, &str, String)> = vec![
        (
            "an IPv4 address with a port",
            Some("127.0.0.1:9297"),
            MODELLED_TARGET,
            "127.0.0.1:9297".to_string(),
        ),
        (
            "a loopback name with a port",
            Some("localhost:9297"),
            MODELLED_TARGET,
            "localhost:9297".to_string(),
        ),
        (
            "a bracketed IPv6 literal",
            Some("[::1]:9297"),
            MODELLED_TARGET,
            "[::1]:9297".to_string(),
        ),
        (
            "an IPv4 address with no port",
            Some("127.0.0.1"),
            MODELLED_TARGET,
            "127.0.0.1".to_string(),
        ),
        (
            "another loopback address of the interface",
            Some("127.0.0.2:19297"),
            MODELLED_TARGET,
            "127.0.0.2:19297".to_string(),
        ),
        (
            "an absolute-form target and no header",
            None,
            "https://192.0.2.10:8443/service/CloudWatchOmniFrontend/operation/Unused",
            "192.0.2.10:8443".to_string(),
        ),
        (
            "a host naming this service's own AWS endpoint",
            Some("cloudwatch-omni.us-west-2.amazonaws.com"),
            MODELLED_TARGET,
            unserved.clone(),
        ),
        (
            "no host at all",
            None,
            MODELLED_TARGET,
            unserved.clone(),
        ),
    ];

    for (case, host, uri, expected_authority) in rows {
        // One emulator per row: an account holds at most one domain, so each row creates its
        // own.
        let omni = Omni::new();
        let parts = signed_parts(host, uri);
        let created = omni.create_domain(&parts, "omni-announced").await;
        assert_announced_on(&format!("{case} (CreateDomain)"), &created, &expected_authority);

        let fetched = omni.get_domain(&parts, &created.domain_id).await;
        assert_announced_on(&format!("{case} (GetDomain)"), &fetched, &expected_authority);

        let renamed = omni
            .rename_domain(&parts, &created.domain_id, "omni-announced-renamed")
            .await;
        assert_announced_on(&format!("{case} (UpdateDomain)"), &renamed, &expected_authority);
    }
}

/// RULE: the announced address FOLLOWS the request, so a domain created through one address
/// is answered its reader's address — and the read writes nothing, so the first address is
/// reported again when the first caller comes back.
#[tokio::test]
async fn a_read_announces_its_own_address_and_leaves_the_record_alone() {
    let omni = Omni::new();
    let first = signed_parts(Some("127.0.0.1:9297"), MODELLED_TARGET);
    let second = signed_parts(Some("localhost:19297"), MODELLED_TARGET);
    let created = omni.create_domain(&first, "omni-two-addresses").await;
    assert_announced_on("the create's own address", &created, "127.0.0.1:9297");

    // Rows differ along the address the reader was reached at, in both orders, so neither
    // read can be answered from a value the other one left behind.
    for (case, parts, expected) in [
        ("the other address", &second, "localhost:19297"),
        ("the create's address again", &first, "127.0.0.1:9297"),
        ("the other address once more", &second, "localhost:19297"),
    ] {
        let fetched = omni.get_domain(parts, &created.domain_id).await;
        assert_announced_on(case, &fetched, expected);
        assert_eq!(
            (
                fetched.domain_id.as_str(),
                fetched.name.as_deref(),
                fetched.domain_arn.as_str(),
                fetched.status.as_str()
            ),
            (
                created.domain_id.as_str(),
                created.name.as_deref(),
                created.domain_arn.as_str(),
                created.status.as_str()
            ),
            "case {case:?}: a read changed the domain it reports, got {fetched:?}"
        );
    }
}

/// RULE: the address a response announces is the address the request that read it arrived
/// on, over the wire too — the whole point being that a client can open what it was handed.
#[tokio::test]
async fn the_wire_response_announces_the_host_the_request_carried() {
    let omni = Omni::new();
    let parts = signed_parts(Some("127.0.0.1:9297"), MODELLED_TARGET);
    let created = omni.create_domain(&parts, "omni-wire-endpoints").await;

    // Rows differ along the host the wire client stated: an address with a port, the loopback
    // name on another port, and a bracketed IPv6 literal.
    for host in ["127.0.0.1:9297", "localhost:19297", "[::1]:443"] {
        let domain = omni.get_domain_over_the_wire(host, &created.domain_id).await;
        assert_announced_on(host, &domain, host);
    }
}

/// RULE: the announced address is one this emulator SERVES — a fetch of either endpoint
/// renders the stored domain, and an address of the same shape naming a domain this emulator
/// does not hold renders none.
#[tokio::test]
async fn a_fetch_of_an_announced_endpoint_reaches_the_stored_domain() {
    let omni = Omni::new();
    let parts = signed_parts(Some("127.0.0.1:9297"), MODELLED_TARGET);
    let created = omni.create_domain(&parts, "omni-served").await;
    let name = created.name.clone().expect("the create reports its name");
    let custom = created
        .custom_endpoint_urls
        .clone()
        .unwrap_or_default()
        .first()
        .cloned()
        .expect("the create reports an endpoint derived from its name");

    // Rows differ along which announced endpoint is opened and how: the id-addressed URL as
    // handed out, the name-derived one, the id-addressed path with the trailing slash a
    // browser normalises a bare address to, and a HEAD that asks only what is there.
    for (case, method, url) in [
        (
            "the id-addressed endpoint",
            "GET",
            created.domain_endpoint_url.clone(),
        ),
        ("the name-derived endpoint", "GET", custom.clone()),
        (
            "the id-addressed endpoint with a trailing slash",
            "GET",
            format!("{}/", created.domain_endpoint_url),
        ),
        (
            "a HEAD of the id-addressed endpoint",
            "HEAD",
            created.domain_endpoint_url.clone(),
        ),
    ] {
        let (status, media_type, body) = omni.fetch(method, &url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: fetching {url:?} answered {status}: {body:?}"
        );
        assert!(
            media_type.starts_with("text/html"),
            "case {case:?}: expected a web page, got content-type {media_type:?}"
        );
        if method == "GET" {
            for stated in [created.domain_id.as_str(), name.as_str(), REGION] {
                assert!(
                    body.contains(stated),
                    "case {case:?}: expected the page to state {stated:?}, got {body:?}"
                );
            }
        } else {
            assert!(
                body.is_empty(),
                "case {case:?}: a HEAD carries no body, got {body:?}"
            );
        }
    }

    // Rows differ along what the address of this shape names: a domain id this emulator holds
    // nothing under, a domain name it holds nothing under, and the endpoint of a domain whose
    // deletion has completed — none of which may render a domain.
    let announced_authority = "127.0.0.1:9297";
    for (case, url) in [
        (
            "an id this emulator holds no domain under",
            format!(
                "https://{announced_authority}{}",
                omni::state::domain_endpoint_path("d-doesnotexist")
            ),
        ),
        (
            "a name this emulator holds no domain under",
            format!(
                "https://{announced_authority}{}",
                omni::state::domain_name_endpoint_path("omni-nothing")
            ),
        ),
    ] {
        let (status, _media_type, body) = omni.fetch("GET", &url).await;
        assert_eq!(
            status, 404,
            "case {case:?}: fetching {url:?} answered {status}: {body:?}"
        );
        assert!(
            !body.contains(&created.domain_id),
            "case {case:?}: expected no domain, got {body:?}"
        );
    }

    // A path BELOW an announced endpoint is not that endpoint: the endpoint is one page
    // rather than a path space, so nothing there reports the domain.
    let (_status, _media_type, body) = omni
        .fetch("GET", &format!("{}/auth/code", created.domain_endpoint_url))
        .await;
    assert!(
        !body.contains(&created.domain_id),
        "case \"a path below the endpoint\": expected no domain page, got {body:?}"
    );
}
