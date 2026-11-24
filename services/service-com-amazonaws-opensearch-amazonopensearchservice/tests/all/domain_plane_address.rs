// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a domain publishes for its own data plane: what shape it has, and what
//! answers there.
//!
//! The rule under test: **`DomainStatus$Endpoint` is an address a client takes as the HOST it
//! is — it signs it, hands it to an OpenSearch client as one, and joins an absolute request
//! path onto it — so the address this emulator publishes for a domain is a bare `host:port`
//! with no scheme and no path, it is that domain's OWN (no two domains publish one address),
//! and the whole OpenSearch REST surface answers at the ROOT of it. An address whose domain is
//! gone answers for no domain at all.**
//!
//! A value carrying a path is unusable to every client that does any of those three, however
//! reachable it is for one that only concatenates — which is why the address is the
//! `host:port` of a listener this emulator opens for the domain
//! ([`service_com_amazonaws_opensearch_amazonopensearchservice::state::domain_plane`]), and
//! why what a case here asserts is the SHAPE of the published value and that dialling it at
//! the root reaches that domain: the host and the port themselves are per-run values.
//!
//! The rows span what the rule generalises over: which domain publishes the address, what is
//! asked at the root of it (the cluster's banner and health, an index create, a document
//! write, a count, a search), whether the request arrives at the front door or ON THE LISTENER
//! the domain published (where there is no operation context, so who the request speaks for is
//! read from the credential it presents), which principal signs it there, and whether the
//! domain behind the address still exists. Every case drives the real wire path — an HTTP
//! request through the multi-service router into the mounted service, or the very function the
//! core hands a request accepted on that listener — addressed at the published value exactly
//! as a client dials it. The control-plane harness comes from [`super::domain_lifecycle`] and
//! the published-address helpers from [`super::published_domain_address`]; this suite adds no
//! second copy of them.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::service::domain_rest::serve_published;

use super::domain_lifecycle::{settled_domain, Answer, Emulator, DEFAULT_ACCESS_KEY_ID, REGION};
use super::published_domain_address::{
    at_address, dialable_authority, given_session, published_endpoint, role_arn,
};

/// The two domains the rows are read on, so an address that reached "a domain" could not be
/// the one domain a single-domain store leaves no choice about.
const DOMAINS: [&str; 2] = ["plane-first-domain", "plane-second-domain"];

/// The session name the seeded credentials of the listener case carry.
const SESSION: &str = "plane-session";

/// The role that case's domain names as its fine-grained-access-control master user, and the
/// access key id of its session.
const MASTER_ROLE: &str = "plane-master-role";
const MASTER_KEY: &str = "ASIAEXAMPLEPLANEMST1";

/// A role of the same account that no role mapping binds, and its session's access key id.
const READER_ROLE: &str = "plane-reader-role";
const READER_KEY: &str = "ASIAEXAMPLEPLANERDR1";

/// `CreateDomain` for one of them: no access policy and no fine-grained access control, so
/// what a row observes is the ADDRESS and not an authorization decision.
fn create_body(domain: &str) -> Value {
    json!({
        "DomainName": domain,
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    })
}

/// The routes asked at the ROOT of a published address, as `(case, method, path, body)` — the
/// whole path belongs to the client, so none of them carries a prefix.
fn routes() -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        ("the cluster banner", "GET", "/".to_string(), None),
        (
            "the cluster health",
            "GET",
            "/_cluster/health".to_string(),
            None,
        ),
        ("an index create", "PUT", "/catalog".to_string(), None),
        (
            "a document write",
            "PUT",
            "/catalog/_doc/one".to_string(),
            Some(json!({ "title": "the plane probe" })),
        ),
        ("a count", "GET", "/catalog/_count".to_string(), None),
        (
            "a search",
            "POST",
            "/catalog/_search".to_string(),
            Some(json!({})),
        ),
    ]
}

/// One OpenSearch REST request as it arrives ON THE LISTENER the domain is published on:
/// the core hands every request accepted there to this one function
/// ([`service_com_amazonaws_opensearch_amazonopensearchservice::state::domain_plane`]), with
/// no operation context, so WHO it speaks for is read from the credential it presents — and
/// with the handle onto this emulator the listener was opened with, which is what a run served
/// there reaches another service through.
async fn through_the_listener(
    emulator: &Emulator,
    domain: &str,
    address: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{address}{path}"))
        .header("host", address)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/es/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .body(Vec::new())
        .expect("the test request is well formed");
    let callback = emulator.callback();
    let answered =
        serve_published(&**emulator.engine(), domain, &request, Some(&callback)).await;
    let status = answered.status().as_u16();
    let body: Value = serde_json::from_slice(answered.body()).unwrap_or(Value::Null);
    (status, body)
}

/// The address `127.0.0.1:<port>` names as a socket address, for the case that opens a
/// connection to it.
fn socket_addr_of(address: &str) -> std::net::SocketAddr {
    use std::net::ToSocketAddrs;
    address
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .unwrap_or_else(|| panic!("the published address {address:?} resolves to no socket"))
}

/// Create `domain`, wait until it reports itself settled, and answer the address it publishes.
async fn given_domain(emulator: &Emulator, domain: &str) -> String {
    let created = emulator.create(create_body(domain)).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({domain:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(emulator, domain).await;
    published_endpoint(emulator, domain, domain).await
}

/// The cluster name an answer about a domain's own cluster carries, which is what tells which
/// domain answered.
fn cluster_name(answer: &Answer) -> String {
    answer.body["cluster_name"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// RULE: every domain publishes a bare `host:port` of its own, and the whole REST surface
/// answers at the root of it — for the domain that published it.
#[tokio::test]
async fn each_domain_publishes_a_bare_authority_its_own_surface_answers_at() {
    let emulator = Emulator::new();
    let mut addresses = Vec::new();
    for domain in DOMAINS {
        addresses.push((domain, given_domain(&emulator, domain).await));
    }

    for (domain, address) in &addresses {
        assert!(
            dialable_authority(address),
            "case {domain:?}: expected a bare host:port with no scheme and no path, got \
             {address:?}"
        );
        for (case, method, path, body) in routes() {
            let answered = at_address(
                &emulator,
                address,
                DEFAULT_ACCESS_KEY_ID,
                method,
                &path,
                body,
            )
            .await;
            assert!(
                (200..300).contains(&answered.status),
                "case {domain:?} / {case:?}: expected the domain's own surface to answer at \
                 the root of {address:?}, got {} with {:?}",
                answered.status,
                answered.body
            );
        }
        // And it is THAT domain's cluster behind the address, not just some domain's: the
        // banner names the cluster the address belongs to.
        let banner = at_address(&emulator, address, DEFAULT_ACCESS_KEY_ID, "GET", "/", None).await;
        assert_eq!(
            cluster_name(&banner),
            format!("{}:{domain}", ::engine::emulator_account_id()),
            "case {domain:?}: expected the address to serve this domain's own cluster, got {} \
             with {:?}",
            banner.status,
            banner.body
        );
    }

    // No two domains publish one address: the address is what identifies the domain, so the
    // whole point is that they differ.
    let published: Vec<&String> = addresses.iter().map(|(_, address)| address).collect();
    assert_ne!(
        published[0], published[1],
        "case \"two domains\": expected each to publish its own address, got {published:?}"
    );

    // And each address is OPEN: a client can connect to it. The listener binds the interface
    // the core chose (loopback unless the operator asked otherwise), so the connection is made
    // to the loopback address of the published port.
    for (domain, address) in &addresses {
        let addr = socket_addr_of(address);
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
            "case {domain:?}: expected the published address {address:?} to be one a client can \
             open, got {opened:?}"
        );
    }
}

/// RULE: what the address answers is the same surface, decided for the same principal, as the
/// front door answers — because a request accepted on the listener is served by the one
/// reading of this surface, with WHO it speaks for read from the credential it presents rather
/// than from an operation context the core does not build.
///
/// The rows span who signs (the session of the role the domain's `MasterUserARN` names, a
/// session of a role no mapping binds, a credential naming nobody) and what each is answered
/// with — so a reading that resolved no principal at all, which would refuse the master too,
/// could not pass.
#[tokio::test]
async fn the_listener_serves_the_surface_for_the_principal_its_credential_names() {
    let emulator = Emulator::new();
    let domain = "plane-listener-domain";
    given_session(&emulator, MASTER_ROLE, SESSION, MASTER_KEY);
    given_session(&emulator, READER_ROLE, SESSION, READER_KEY);
    let created = emulator
        .create({
            let mut body = create_body(domain);
            body["EncryptionAtRestOptions"] = json!({ "Enabled": true });
            body["NodeToNodeEncryptionOptions"] = json!({ "Enabled": true });
            body["DomainEndpointOptions"] = json!({ "EnforceHTTPS": true });
            body["AdvancedSecurityOptions"] = json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": false,
                "MasterUserOptions": { "MasterUserARN": role_arn(MASTER_ROLE) },
            });
            body
        })
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({domain:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, domain).await;
    let address = published_endpoint(&emulator, domain, domain).await;

    // The master's session is served, and the answer carries the cluster-health document this
    // domain's own configuration decides — so neither the status nor the body is lost on the
    // way out of the listener.
    let (status, health) = through_the_listener(
        &emulator,
        domain,
        &address,
        MASTER_KEY,
        "GET",
        "/_cluster/health",
    )
    .await;
    assert_eq!(
        (status, health["cluster_name"].as_str()),
        (
            200,
            Some(format!("{}:{domain}", ::engine::emulator_account_id()).as_str())
        ),
        "case \"the master role's session on the listener\": expected this domain's own cluster \
         health, got {status} with {health:?}"
    );

    // A session of a role no mapping binds reaches the same surface and is refused by the
    // CLUSTER, which is the decision the principal resolved for it earns.
    let (status, refused) = through_the_listener(
        &emulator,
        domain,
        &address,
        READER_KEY,
        "POST",
        "/catalog/_search",
    )
    .await;
    assert_eq!(
        (status, refused["error"]["type"].as_str()),
        (403, Some("security_exception")),
        "case \"a session no mapping binds on the listener\": expected the cluster's own \
         refusal, got {status} with {refused:?}"
    );

    // A credential this emulator can resolve no principal for is nobody on a domain whose
    // cluster admits only the principals it binds, so it is refused the same way.
    let (status, nobody) = through_the_listener(
        &emulator,
        domain,
        &address,
        "ASIAEXAMPLENOBODY001",
        "GET",
        "/_cluster/health",
    )
    .await;
    assert_eq!(
        (status, nobody["error"]["type"].as_str()),
        (403, Some("security_exception")),
        "case \"a credential naming nobody on the listener\": expected the cluster's own \
         refusal, got {status} with {nobody:?}"
    );
}

/// RULE: an address whose domain is gone answers for no domain — the address is withdrawn with
/// the domain that published it, so nothing answers there rather than some other domain's
/// cluster.
#[tokio::test]
async fn a_deleted_domains_address_answers_for_no_domain() {
    let emulator = Emulator::new();
    let domain = "plane-withdrawn-domain";
    let address = given_domain(&emulator, domain).await;
    let served = at_address(&emulator, &address, DEFAULT_ACCESS_KEY_ID, "GET", "/", None).await;
    assert_eq!(
        served.status, 200,
        "case \"before the delete\": expected the published address to serve the domain, got \
         {} with {:?}",
        served.status, served.body
    );

    let deleted = emulator.delete(domain).await;
    assert_eq!(
        deleted.status, 200,
        "case \"DeleteDomain\": expected 200, got {} with {:?}",
        deleted.status, deleted.body
    );
    let answered = at_address(&emulator, &address, DEFAULT_ACCESS_KEY_ID, "GET", "/", None).await;
    assert!(
        !(200..300).contains(&answered.status),
        "case \"after the delete\": expected the address to answer for no domain, got {} with \
         {:?}",
        answered.status,
        answered.body
    );
}
