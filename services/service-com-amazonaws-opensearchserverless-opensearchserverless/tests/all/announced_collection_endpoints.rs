// SPDX-License-Identifier: CC0-1.0

//! The addresses a collection PUBLISHES, and that every one of them is an address this
//! emulator answers the collection's OpenSearch REST surface on.
//!
//! `CollectionDetail$collectionEndpoint` is the "Collection-specific endpoint used to
//! submit index, search, and data upload requests to an OpenSearch Serverless collection",
//! and `dashboardEndpoint` / `fipsEndpoints` are the same address in its other forms: none
//! of them is a label a caller reads back, each is an address a client is CONFIGURED with
//! and then DIALS at its root — which is what AWS's own documented
//! `OpenSearch(hosts=[{'host': <collectionEndpoint host>, 'port': 443}], …)` client does.
//!
//! So the rule under test is that every published endpoint names an address THIS emulator
//! answers on — the collection's own listener, opened through the core — rather than AWS's
//! `<id>.<region>.aoss.amazonaws.com` host, which resolves nowhere on the machine the
//! emulator runs on. Each case asserts it three ways: on the value `BatchGetCollection`
//! reports, by OPENING a connection to the address, and by driving the OpenSearch REST
//! surface the listener serves there (`state::collection_plane` hands every request to
//! `service::collection_rest::serve_published`, which is the one decision behind it).
//!
//! The table spans what the rule generalises over: the collection type, which address the
//! creating client reached this emulator at, and whether this process is served on a port
//! at all — none of which may change that the published address is one this emulator
//! answers on.
//!
//! A published address is a `host:port`, and nothing can be read off one, so the last suite
//! below pins the other half of "an address a client can use": that the service answering
//! there is RESOLVABLE — recorded under this service's own signing name in the engine-wide
//! register of published data planes, for exactly as long as the port is open — which is
//! what lets a consumer inside this emulator that was handed a `collectionEndpoint` sign and
//! route a request to it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_common::published_data_planes::PublishedPlanes;
use service_com_amazonaws_opensearchserverless_opensearchserverless::service::collection_rest::serve_published;
use service_com_amazonaws_opensearchserverless_opensearchserverless::state::service_state;
use service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::collection_lifecycle::text;

/// This service's SigV4 signing name, which is what routes a control-plane request to it
/// and the name a client signs a data-plane request with
/// (`smithy_query(query="service_info")` -> `sigv4Name`).
pub(crate) const SIGNING_NAME: &str = "aoss";

/// The region every request below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from this crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "OpenSearchServerless";

/// The path AWS's OpenSearch Dashboards endpoint adds to a collection endpoint.
pub(crate) const DASHBOARDS_PATH: &str = "/_dashboards";

/// One emulator carrying only this service, with the engine the handlers write kept so a
/// test can drive the address a collection is published on, and served at `base` when the
/// emulator is served on a port at all (`MultiServiceRouter::set_url_base`, which is what
/// `OperationContext::endpoint` reports).
pub(crate) fn emulator_served_at(
    base: Option<&str>,
) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the published address, not SigV4: a request carries a credential
    // scope so the router can name the target service and the region, but no signature.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    if let Some(base) = base {
        router.set_url_base(
            EndpointUrl::parse(base).unwrap_or_else(|| panic!("the base URL {base} must parse")),
        );
    }
    (router, engine)
}

/// Call `operation` with `body` as a client that addressed `host`; answer its HTTP status
/// and parsed body.
pub(crate) async fn call_at(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The `CollectionDetail` `BatchGetCollection` reports for `id`, read as a client that
/// addressed `host`.
async fn read_collection(router: &MultiServiceRouter, host: &str, id: &str) -> Value {
    let (status, read) = call_at(router, host, "BatchGetCollection", json!({ "ids": [id] })).await;
    assert_eq!(
        status, 200,
        "case BatchGetCollection({id}) at {host}: expected 200, got {status} with {read}"
    );
    read.get("collectionDetails")
        .and_then(Value::as_array)
        .and_then(|details| details.first())
        .cloned()
        .unwrap_or(Value::Null)
}

/// Create a collection of `kind` as a client that addressed `host`, and poll until it is
/// ACTIVE; answer the `CollectionDetail` that read reports.
///
/// The `CREATING -> ACTIVE` move is owed by the clock, so this looks again until the
/// collection reports ACTIVE or a generous deadline passes; nothing asserts on how long it
/// took.
async fn active_collection_at(
    router: &MultiServiceRouter,
    host: &str,
    name: &str,
    kind: &str,
) -> Value {
    // A collection's endpoints answer only through a `network` security policy that names
    // it — AWS's "Network access for Amazon OpenSearch Serverless": "you must have at least
    // one network policy that applies to a collection in order to access its endpoint" — so
    // the suite admits traffic to every collection of this emulator first.
    admit_network_access(router, host).await;
    let (status, created) = call_at(
        router,
        host,
        "CreateCollection",
        json!({ "name": name, "type": kind, "encryptionConfig": { "aWSOwnedKey": true } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateCollection({name}) at {host}: expected 200, got {status} with {created}"
    );
    let id = text(&created, "createCollectionDetail.id");
    await_active(router, host, &id).await
}

/// Admit traffic to the endpoints of every collection of this emulator. Idempotent, so a
/// suite that creates several collections calls it per collection.
pub(crate) async fn admit_network_access(router: &MultiServiceRouter, host: &str) {
    /// The one network policy this suite reaches its collections through.
    const NETWORK_POLICY_NAME: &str = "net-every-collection";

    let (held, _) = call_at(
        router,
        host,
        "GetSecurityPolicy",
        json!({ "type": "network", "name": NETWORK_POLICY_NAME }),
    )
    .await;
    if held == 200 {
        return;
    }
    let policy = json!({
        "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
        "AllowFromPublic": true,
    });
    let (status, answer) = call_at(
        router,
        host,
        "CreateSecurityPolicy",
        json!({
            "type": "network",
            "name": NETWORK_POLICY_NAME,
            "policy": policy.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateSecurityPolicy(network/{NETWORK_POLICY_NAME}) at {host}: expected 200, \
         got {status} with {answer}"
    );
}

/// Poll `BatchGetCollection` until `id` is ACTIVE, answering the `CollectionDetail` that
/// read reports.
///
/// The `CREATING -> ACTIVE` move is owed by the clock, so this looks again until the
/// collection reports ACTIVE or a generous deadline passes; nothing asserts on how long it
/// took, and the deadline is long enough that a loaded host polling several collections
/// still reaches it.
pub(crate) async fn await_active(router: &MultiServiceRouter, host: &str, id: &str) -> Value {
    /// How long a collection is given to settle before the wait is called a failure:
    /// several times over the few seconds the `CREATING -> ACTIVE` move is owed in, so a
    /// busy host still reaches the condition, and short enough that a collection which
    /// never settles fails the suite promptly.
    const DEADLINE: Duration = Duration::from_secs(15);

    let started = tokio::time::Instant::now();
    loop {
        let detail = read_collection(router, host, id).await;
        if text(&detail, "status") == "ACTIVE" {
            return detail;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {id} at {host}: the collection never reached ACTIVE, last read {detail}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The four endpoints a collection publishes, in the order `CollectionDetail` carries
/// them: the collection endpoint, its Dashboards endpoint, and the FIPS pair.
pub(crate) fn published(detail: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("collectionEndpoint", text(detail, "collectionEndpoint")),
        ("dashboardEndpoint", text(detail, "dashboardEndpoint")),
        (
            "fipsEndpoints.collectionEndpoint",
            text(detail, "fipsEndpoints.collectionEndpoint"),
        ),
        (
            "fipsEndpoints.dashboardEndpoint",
            text(detail, "fipsEndpoints.dashboardEndpoint"),
        ),
    ]
}

/// Assert that every endpoint `detail` publishes is an address THIS emulator answers the
/// collection on, and answer its collection endpoint — the one address the four members
/// name.
///
/// The one statement of the rule for every suite that reads a `CollectionDetail`: each
/// member is an HTTPS address naming no AWS host, carrying the port it is served on and the
/// authority of this collection's own address; the Dashboards endpoint is the collection
/// endpoint plus the documented path, the FIPS pair names that same address, and a client
/// can OPEN every one of them.
pub(crate) fn assert_published_addresses(case: &str, detail: &Value) -> String {
    let endpoint = text(detail, "collectionEndpoint");
    for (member, url) in published(detail) {
        assert!(
            url.starts_with("https://"),
            "case {case:?}: {member} must be an HTTPS address, got {url:?}"
        );
        // AWS's own `<id>.<region>.aoss[-fips].amazonaws.com` hosts resolve nowhere on the
        // machine this emulator runs on, so no published address may name one.
        assert!(
            !url.contains("amazonaws.com"),
            "case {case:?}: {member} must not name an AWS host, got {url:?}"
        );
        let authority = authority_of(&url);
        assert!(
            authority
                .rsplit(':')
                .next()
                .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port != 0)),
            "case {case:?}: {member} must name the port it is served on, got {url:?}"
        );
        assert_eq!(
            authority,
            authority_of(&endpoint),
            "case {case:?}: {member} must name the address this collection is served at \
             ({}), got {url:?}",
            authority_of(&endpoint)
        );
        // The address is OPEN: a client can connect to it. The listener binds the interface
        // the core chose (loopback unless the operator asked otherwise), so the connection
        // is made to the loopback address of the published port.
        let addr = socket_addr_of(&url);
        let opened = match addr.ip() {
            std::net::IpAddr::V4(_) => {
                std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, addr.port()))
            }
            std::net::IpAddr::V6(_) => {
                std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, addr.port()))
            }
        };
        assert!(
            opened.is_ok(),
            "case {case:?}: {member} must be an address a client can open, got {addr} \
             refusing the connection: {opened:?}"
        );
    }
    // The Dashboards endpoint is the collection endpoint plus the documented path, and the
    // FIPS pair names the one address this emulator serves the collection at.
    let expected: Vec<String> = vec![
        endpoint.clone(),
        format!("{endpoint}{DASHBOARDS_PATH}"),
        endpoint.clone(),
        format!("{endpoint}{DASHBOARDS_PATH}"),
    ];
    let reported: Vec<String> = published(detail)
        .into_iter()
        .map(|(_member, url)| url)
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the published endpoints {expected:?}, got {reported:?}"
    );
    endpoint
}

/// The `host:port` a published endpoint addresses — the authority a client dials, with any
/// path dropped.
pub(crate) fn authority_of(url: &str) -> String {
    let after_scheme = url.strip_prefix("https://").unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

/// The socket address a published endpoint names, so a test can open a connection to it.
pub(crate) fn socket_addr_of(url: &str) -> std::net::SocketAddr {
    use std::net::ToSocketAddrs;

    let authority = authority_of(url);
    authority
        .to_socket_addrs()
        .unwrap_or_else(|error| panic!("endpoint {url:?} has no socket address: {error}"))
        .next()
        .unwrap_or_else(|| panic!("endpoint {url:?} resolves to no socket address"))
}

/// One OpenSearch REST request a client sends to a published endpoint: `method path` with
/// `body`, answered by the listener that endpoint is published on
/// (`state::collection_plane` hands every request there to this one function).
pub(crate) fn through_endpoint(
    engine: &Arc<engine::InMemoryEngine>,
    collection_id: &str,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let state = service_state(&**engine).expect("the service store is reachable");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{endpoint}{path}"))
        .header("host", authority_of(endpoint))
        .header("content-type", "application/json")
        .body(
            body.map(|body| body.to_string().into_bytes())
                .unwrap_or_default(),
        )
        .expect("a well-formed request to a collection endpoint");
    let response = serve_published(&**engine, &state, collection_id, &request);
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One OpenSearch REST request sent to the emulator's own FRONT DOOR addressed to `host`,
/// which is how a client reaches a collection whose published authority it carries in the
/// `Host` header: no RPC target, because the OpenSearch REST API has none.
pub(crate) async fn rest_at_front_door(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDDATAPLANE/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A `knn_vector` index body of `dimensions` dimensions — the index a Bedrock knowledge/// base requires to exist in the collection, and the body the recorded `indices.create`
/// carries.
pub(crate) fn knn_index_body(dimensions: usize) -> Value {
    json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            "bedrock-knowledge-base-default-vector": {
                "type": "knn_vector",
                "dimension": dimensions,
            },
            "AMAZON_BEDROCK_TEXT_CHUNK": { "type": "text" },
        } },
    })
}

/// RULE: every endpoint a collection publishes names an address this emulator answers the
/// collection's OpenSearch REST surface on, at the ROOT of that address — so a client
/// configured with the published host and port reaches the collection with no prefix it
/// would have to be told about — and no published endpoint names one of AWS's own hosts.
///
/// The rows span the collection type, the address the creating client reached this emulator
/// at, and whether this process is served on a port at all.
#[tokio::test]
async fn every_published_collection_endpoint_is_an_address_this_emulator_serves() {
    /// One case: the collection created, who created it and where the emulator is served.
    struct Row {
        case: &'static str,
        kind: &'static str,
        caller_host: &'static str,
        served_base: Option<&'static str>,
    }

    let aws_host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let rows: Vec<Row> = vec![
        Row {
            case: "a vector collection created from a client on the emulator's own endpoint",
            kind: "VECTORSEARCH",
            caller_host: "127.0.0.1:9297",
            served_base: Some("https://127.0.0.1:39355"),
        },
        Row {
            case: "a search collection created from a client that resolves AWS's own hosts",
            kind: "SEARCH",
            caller_host: "aws-host",
            served_base: Some("https://127.0.0.1:39355"),
        },
        Row {
            case: "a time-series collection on an emulator served on no port at all",
            kind: "TIMESERIES",
            caller_host: "aws-host",
            served_base: None,
        },
        Row {
            case: "a vector collection created from a loopback name",
            kind: "VECTORSEARCH",
            caller_host: "localhost:9297",
            served_base: None,
        },
    ];

    for Row {
        case,
        kind,
        caller_host,
        served_base,
    } in rows
    {
        let (router, engine) = emulator_served_at(served_base);
        let host = match caller_host {
            "aws-host" => aws_host.clone(),
            named => named.to_string(),
        };
        let detail = active_collection_at(&router, &host, "announced-endpoints", kind).await;
        let id = text(&detail, "id");
        let endpoint = text(&detail, "collectionEndpoint");

        // 1. Every published endpoint is an address this emulator answers on, none of them
        //    is one of AWS's own hosts, and each is OPEN.
        assert_published_addresses(case, &detail);

        // 2. The address SERVES the collection's OpenSearch REST surface at its root: the
        //    vector index a knowledge base requires is created there and read back.
        let index = "bedrock-knowledge-base-index";
        let (status, created) = through_endpoint(
            &engine,
            &id,
            &endpoint,
            "PUT",
            &format!("/{index}"),
            Some(&knn_index_body(1536)),
        );
        assert_eq!(
            (status, created.get("acknowledged")),
            (200, Some(&json!(true))),
            "case {case:?}: PUT {endpoint}/{index} expected 200 acknowledged, got {status} with \
             {created}"
        );
        let (status, read) = through_endpoint(
            &engine,
            &id,
            &endpoint,
            "GET",
            &format!("/{index}"),
            None,
        );
        assert_eq!(
            status, 200,
            "case {case:?}: GET {endpoint}/{index} expected 200, got {status} with {read}"
        );
        assert!(
            read.get(index).is_some(),
            "case {case:?}: GET {endpoint}/{index} must report the index, got {read}"
        );

        // 3. The emulator's own FRONT DOOR answers the same request addressed to the
        //    collection's published authority, port and all — the addressing a client that
        //    was handed the endpoint earlier, or that reaches this process on one socket,
        //    still sends.
        let (status, read) = rest_at_front_door(
            &router,
            &authority_of(&endpoint),
            "GET",
            &format!("/{index}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GET /{index} at the front door for host {:?} expected 200, got \
             {status} with {read}",
            authority_of(&endpoint)
        );
        assert!(
            read.get(index).is_some(),
            "case {case:?}: the front door must report the index of the collection the \
             published authority names, got {read}"
        );

        // 4. The control plane keeps reporting the same addresses on a later read, so a
        //    client that stored the endpoint keeps dialling the one this emulator serves.
        let again = read_collection(&router, &host, &id).await;
        assert_eq!(
            published(&again),
            published(&detail),
            "case {case:?}: a later read must report the same addresses, got {again}"
        );
    }
}

/// RULE: a published endpoint addresses ONE collection, so two collections of the same
/// emulator publish two different addresses and each serves only its own indexes.
#[tokio::test]
async fn each_collection_publishes_an_address_of_its_own() {
    let (router, engine) = emulator_served_at(Some("https://127.0.0.1:39355"));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let first = active_collection_at(&router, &host, "published-first", "VECTORSEARCH").await;
    let second = active_collection_at(&router, &host, "published-second", "VECTORSEARCH").await;
    let first_endpoint = text(&first, "collectionEndpoint");
    let second_endpoint = text(&second, "collectionEndpoint");
    assert_ne!(
        first_endpoint, second_endpoint,
        "case two collections: each must publish an address of its own, got \
         {first_endpoint:?} twice"
    );

    let index = "only-in-first";
    let (status, created) = through_endpoint(
        &engine,
        &text(&first, "id"),
        &first_endpoint,
        "PUT",
        &format!("/{index}"),
        Some(&knn_index_body(8)),
    );
    assert_eq!(
        status, 200,
        "case PUT {first_endpoint}/{index}: expected 200, got {status} with {created}"
    );

    // (case, the collection read, the endpoint it publishes, the status the read answers)
    let rows: Vec<(&str, &Value, &String, u16)> = vec![
        (
            "the collection that holds the index",
            &first,
            &first_endpoint,
            200,
        ),
        (
            "the collection that does not",
            &second,
            &second_endpoint,
            404,
        ),
    ];
    for (case, collection, endpoint, expected) in rows {
        let (status, read) = through_endpoint(
            &engine,
            &text(collection, "id"),
            endpoint,
            "GET",
            &format!("/{index}"),
            None,
        );
        assert_eq!(
            status, expected,
            "case {case}: GET {endpoint}/{index} expected {expected}, got {status} with {read}"
        );
    }
}

/// RULE: every address a collection publishes is recorded, in the engine-wide register of
/// published data planes, as an address THIS service answers at — so a consumer inside this
/// emulator that was handed one, off whose `host:port` nothing can be read, resolves the
/// service to route and sign a request to — and that record lasts exactly as long as the
/// port does: it is withdrawn when the collection is purged.
///
/// The rows span what the rule generalises over: the collection type behind the address,
/// several collections published at once, each of the four endpoint members a collection
/// reports, and the addresses that resolve to nothing (a port nothing was published at, a
/// host nothing was published at, and the address of a collection that is gone).
#[tokio::test]
async fn a_published_collection_address_resolves_to_this_service_until_its_port_closes() {
    let (router, engine) = emulator_served_at(Some("https://127.0.0.1:39355"));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let register = PublishedPlanes::of(&*engine).expect("the engine vends the register");

    // One collection of each type the model names, so the rule is read on every kind of
    // collection and one delete can be shown not to disturb the others.
    let kinds: [(&str, &str); 3] = [
        ("a vector collection", "VECTORSEARCH"),
        ("a search collection", "SEARCH"),
        ("a time-series collection", "TIMESERIES"),
    ];
    let mut collections: Vec<(&str, String, Value)> = Vec::new();
    for (nth, (case, kind)) in kinds.iter().enumerate() {
        let detail = active_collection_at(&router, &host, &format!("resolvable-{nth}"), kind).await;
        collections.push((case, text(&detail, "id"), detail));
    }

    // Every endpoint member of every collection resolves to this service: the four members
    // name the one address that collection is served at, and that address answers here.
    for (case, id, detail) in &collections {
        for (member, url) in published(detail) {
            let authority = authority_of(&url);
            let answering = register.answering_service(&authority);
            assert_eq!(
                answering.as_deref(),
                Some(SIGNING_NAME),
                "case {case:?} ({id}): expected {member} at {authority:?} to resolve to \
                 {SIGNING_NAME:?}, got {answering:?}"
            );
        }
    }

    // Nothing resolves at an address this emulator published nothing at: the register keys
    // one record per listener, port and all.
    let opened: Vec<String> = collections
        .iter()
        .map(|(_case, _id, detail)| authority_of(&text(detail, "collectionEndpoint")))
        .collect();
    let first_authority = opened
        .first()
        .cloned()
        .expect("the suite created collections");
    let unpublished: [(&str, String); 2] = [
        (
            "a host nothing was published at",
            "10.255.255.1:443".to_string(),
        ),
        (
            "another port of the same host",
            a_port_none_of_these_answer_on(&first_authority, &opened),
        ),
    ];
    for (case, authority) in &unpublished {
        let answering = register.answering_service(authority);
        assert_eq!(
            answering, None,
            "case {case:?}: expected {authority:?} to resolve to nothing, got {answering:?}"
        );
    }

    // The purge closes the port, so the record goes with it — and every address this service
    // still answers on keeps resolving.
    let (case, id, detail) = collections
        .first()
        .cloned()
        .expect("the suite created collections");
    let authority = authority_of(&text(&detail, "collectionEndpoint"));
    let (status, deleted) = call_at(&router, &host, "DeleteCollection", json!({ "id": &id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteCollection({id}) expected 200, got {status} with {deleted}"
    );
    await_purged(&router, &host, &id).await;
    let answering = register.answering_service(&authority);
    assert_eq!(
        answering, None,
        "case {case:?}: expected the purged collection's address {authority:?} to resolve to \
         nothing, got {answering:?}"
    );
    for (other_case, other_id, other_detail) in collections.iter().skip(1) {
        let other_authority = authority_of(&text(other_detail, "collectionEndpoint"));
        let answering = register.answering_service(&other_authority);
        assert_eq!(
            answering.as_deref(),
            Some(SIGNING_NAME),
            "case {other_case:?} ({other_id}): expected the surviving address \
             {other_authority:?} to still resolve to {SIGNING_NAME:?}, got {answering:?}"
        );
    }
}

/// Poll `BatchGetCollection` until `id` answers nothing, which is when the deleted
/// collection has been purged.
///
/// The purge is owed by the clock, so this looks again until the condition holds or a
/// generous deadline passes; nothing asserts on how long it took.
async fn await_purged(router: &MultiServiceRouter, host: &str, id: &str) {
    /// Several times over the few seconds the `DELETING -> gone` move is owed in, so a busy
    /// host still reaches the condition.
    const DEADLINE: Duration = Duration::from_secs(15);

    let started = tokio::time::Instant::now();
    loop {
        let detail = read_collection(router, host, id).await;
        if detail.is_null() {
            return;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {id}: the collection was never purged, last read {detail}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The same host as `authority` at a port none of `opened` answers on — the addresses this
/// suite itself opened being every one this engine has published, because the only planes
/// published on this engine are the collections created above.
///
/// The port is walked upward rather than taken as `+1`: the operating system hands out
/// ephemeral ports in runs, so the next port along can be a sibling listener's, and a probe
/// that happened to name one would fail for something other than the rule under test. A port
/// past the last one a port number can hold is skipped, so every probe stays an address a
/// client could dial.
fn a_port_none_of_these_answer_on(authority: &str, opened: &[String]) -> String {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("a published address carries a port, got {authority:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|_| panic!("a published address carries a numeric port, got {port:?}"));
    // Bounded by one more than the addresses there are: each can rule out at most one port.
    for step in 1..=(opened.len() as u16 + 1) {
        let Some(probe_port) = port.checked_add(step) else {
            break;
        };
        let probe = format!("{host}:{probe_port}");
        if !opened.iter().any(|taken| taken.eq_ignore_ascii_case(&probe)) {
            return probe;
        }
    }
    // Every port above the published one is taken or out of range: walk DOWNWARD instead,
    // which is the same question asked of the other side of that port.
    for step in 1..=(opened.len() as u16 + 1) {
        let Some(probe_port) = port.checked_sub(step) else {
            break;
        };
        let probe = format!("{host}:{probe_port}");
        if !opened.iter().any(|taken| taken.eq_ignore_ascii_case(&probe)) {
            return probe;
        }
    }
    panic!(
        "a port none of {opened:?} answers on exists within {} steps of {authority:?}",
        opened.len() + 1
    )
}

/// RULE: an address is published only while the collection behind it exists — once a
/// deleted collection is purged, the address this service announced for it serves nothing.
#[tokio::test]
async fn a_purged_collections_address_stops_serving() {
    let (router, engine) = emulator_served_at(Some("https://127.0.0.1:39355"));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let collection = active_collection_at(&router, &host, "published-deleted", "SEARCH").await;
    let id = text(&collection, "id");
    let endpoint = text(&collection, "collectionEndpoint");
    let index = "before-the-delete";
    let (status, created) = through_endpoint(
        &engine,
        &id,
        &endpoint,
        "PUT",
        &format!("/{index}"),
        Some(&knn_index_body(4)),
    );
    assert_eq!(
        status, 200,
        "case PUT {endpoint}/{index}: expected 200, got {status} with {created}"
    );

    let (status, deleted) = call_at(&router, &host, "DeleteCollection", json!({ "id": &id })).await;
    assert_eq!(
        status, 200,
        "case DeleteCollection({id}): expected 200, got {status} with {deleted}"
    );

    // The purge is owed by the clock: wait for the collection to be gone (the suite's one
    // purge-wait), after which the address it was published on serves nothing.
    await_purged(&router, &host, &id).await;
    let (status, answer) =
        through_endpoint(&engine, &id, &endpoint, "GET", &format!("/{index}"), None);
    assert_eq!(
        status, 503,
        "case GET {endpoint}/{index} after the purge: expected 503, got {status} with {answer}"
    );
}

/// RULE: a collection keeps the address it was published at, so a client that was handed an
/// endpoint keeps reaching the collection there — a retried create (a replayed
/// `clientToken`) answers the collection the first call created and moves nothing.
///
/// The rows span what a retry can carry beside the token: the same request again, and one
/// that names the collection's other members too.
#[tokio::test]
async fn a_retried_create_keeps_the_address_the_first_call_published() {
    let (router, engine) = emulator_served_at(Some("https://127.0.0.1:39355"));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let name = "published-retried";
    let token = "published-retried-token";
    // As for every collection this suite addresses: its endpoints answer only through a
    // network policy that names it.
    admit_network_access(&router, &host).await;
    let request = json!({
        "name": name,
        "type": "VECTORSEARCH",
        "encryptionConfig": { "aWSOwnedKey": true },
        "clientToken": token,
    });
    let (status, created) = call_at(&router, &host, "CreateCollection", request.clone()).await;
    assert_eq!(
        status, 200,
        "case CreateCollection({name}): expected 200, got {status} with {created}"
    );
    let id = text(&created, "createCollectionDetail.id");
    let first = await_active(&router, &host, &id).await;
    let endpoint = text(&first, "collectionEndpoint");
    let index = "survives-the-retry";
    let (status, answer) = through_endpoint(
        &engine,
        &id,
        &endpoint,
        "PUT",
        &format!("/{index}"),
        Some(&knn_index_body(16)),
    );
    assert_eq!(
        status, 200,
        "case PUT {endpoint}/{index}: expected 200, got {status} with {answer}"
    );

    // (case, the retried request)
    let retries: Vec<(&str, Value)> = vec![
        ("the same request again", request.clone()),
        (
            "a retry naming the other members too",
            json!({
                "name": name,
                "type": "VECTORSEARCH",
                "encryptionConfig": { "aWSOwnedKey": true },
                "standbyReplicas": "DISABLED",
                "clientToken": token,
            }),
        ),
    ];
    for (case, retried) in retries {
        let (status, replayed) = call_at(&router, &host, "CreateCollection", retried).await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200, got {status} with {replayed}"
        );
        assert_eq!(
            text(&replayed, "createCollectionDetail.id"),
            id,
            "case {case}: the retry must answer the collection the first call created, got \
             {replayed}"
        );
        let read = read_collection(&router, &host, &id).await;
        assert_eq!(
            published(&read),
            published(&first),
            "case {case}: the published addresses must not move, got {read}"
        );
        // The address still serves what was written through it, which is what a client
        // dialling the endpoint it was handed sees.
        let (status, answer) = through_endpoint(
            &engine,
            &id,
            &endpoint,
            "GET",
            &format!("/{index}"),
            None,
        );
        assert_eq!(
            status, 200,
            "case {case}: GET {endpoint}/{index} expected 200, got {status} with {answer}"
        );
        let addr = socket_addr_of(&endpoint);
        let opened = match addr.ip() {
            std::net::IpAddr::V4(_) => {
                std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, addr.port()))
            }
            std::net::IpAddr::V6(_) => {
                std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, addr.port()))
            }
        };
        assert!(
            opened.is_ok(),
            "case {case}: the published port must still be open, got {addr} refusing the \
             connection: {opened:?}"
        );
    }
}
