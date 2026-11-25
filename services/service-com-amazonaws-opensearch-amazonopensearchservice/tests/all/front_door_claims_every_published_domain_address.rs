// SPDX-License-Identifier: CC0-1.0

//! Which ADDRESSES reach a domain's OpenSearch REST data plane through this emulator's front
//! door — and which reach no service at all.
//!
//! RULE, one level above the recorded case: **a request signed for this service's routing key
//! and addressed to an address the control plane PUBLISHED for a domain this store holds is
//! claimed by this service and answered by that domain's REST data plane, so what the client
//! sees is the CLUSTER's own verdict — its answer for the master user and for a principal a
//! role mapping binds, its `403 security_exception` for a principal no mapping binds — while
//! the same request to an address no domain of this store publishes is claimed by nothing and
//! answered by the front door's `404 UnknownService`.**
//!
//! Every address the control plane publishes for a domain's data plane is one a client is
//! CONFIGURED with — `DomainStatus$Endpoint` is "the domain-specific endpoint used to submit
//! index, search, and data upload requests to the domain", and `EndpointV2` and `Endpoints`
//! are that same endpoint for a domain reached over both address families or inside a VPC — so
//! an address that reaches no data plane is an address no client can use: the poll a client
//! opens with, `GET /_cluster/health` on the host it read out of `DescribeDomain`, would never
//! answer, and an authorization outcome would be told as a routing fault.
//!
//! The rows span what the rule generalises over: WHICH published address is dialled (the value
//! `Endpoint` carries, dialled exactly as published; the public host AWS names that endpoint
//! by; the dual-stack host `EndpointV2` reports; the two hosts `Endpoints` reports for a
//! VPC-access domain; and the prefixed path-style address, naming the domain by its own name
//! and by a host it publishes), WHICH principal signs (the master user, a role no mapping
//! binds, that same role once a mapping binds it) and WHAT it asks for (the cluster health and
//! a search). The last test's rows span the addresses that name no domain of this store.
//!
//! The harness is this crate's own ([`super::domain_lifecycle`]); the seeded session, its IAM
//! grant and the signed data-plane request come from [`super::domain_rest_data_plane`] rather
//! than a second copy of them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state;

use super::domain_lifecycle::{error_code, Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_data_plane::{
    delivery_role_arn, dispatch_rest, endpoint_of, error_type, given_data_plane_policy,
    given_session, SESSION_KEY_ID,
};

/// The host AWS publishes this service's API at for the Region the harness signs for. It is
/// what a prefixed path-style request carries, because that addressing names the plane and the
/// domain in its PATH and is resolved whatever host it arrived on.
const SERVICE_HOST: &str = "es.us-east-1.amazonaws.com";

/// The index the search rows address, created by the master user.
const INDEX: &str = "catalog";

/// How long a poll waits for the control plane to publish a domain's address before it gives
/// up. Generous on purpose: no case here is about how long provisioning takes, so this is only
/// the point at which waiting is called a failure.
const PUBLICATION_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking at the domain again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// `CreateDomain` with fine-grained access control switched on and its master user named by
/// the identity the harness's default credential speaks for, plus the members `extra` carries
/// for the access mode a test wants.
fn fgac_create(name: &str, extra: &Value) -> Value {
    let mut body = json!({
        "DomainName": name,
        "EncryptionAtRestOptions": { "Enabled": true },
        "NodeToNodeEncryptionOptions": { "Enabled": true },
        "DomainEndpointOptions": { "EnforceHTTPS": true },
        "AdvancedSecurityOptions": {
            "Enabled": true,
            "InternalUserDatabaseEnabled": false,
            "MasterUserOptions": {
                "MasterUserARN": ::service_common::caller_principal::ambient_user_arn()
            }
        }
    });
    if let Value::Object(members) = extra {
        for (member, value) in members {
            body[member.as_str()] = value.clone();
        }
    }
    body
}

/// Create that domain and answer its `DomainStatus` once the control plane publishes `member`
/// for it — the address a client is configured with, which a domain still being provisioned
/// publishes not at all.
///
/// The wait is for the CONDITION the client waits for (the address being published) rather
/// than for a span, so nothing here depends on how fast the host runs.
async fn published(emulator: &Emulator, name: &str, extra: &Value, member: &str) -> Value {
    let created = emulator.create(fgac_create(name, extra)).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({name:?}) with fine-grained access control\": expected 200, got \
         {} with {:?}",
        created.status, created.body
    );
    let deadline = Instant::now() + PUBLICATION_DEADLINE;
    loop {
        let answer = emulator.describe(name).await;
        let status = answer.body["DomainStatus"].clone();
        if !status[member].is_null() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case \"DescribeDomain({name:?})\": the domain never published a {member}, last \
             read {} with {:?}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One ADDRESS a client reaches a domain's data plane by: the authority its requests carry,
/// and the path prefix that address puts in front of the client's own request path.
struct Addressing {
    case: String,
    authority: String,
    prefix: String,
}

impl Addressing {
    /// A value the control plane PUBLISHED, dialled exactly as published: a client takes the
    /// authority as the host it sends to and joins its request path onto whatever the value
    /// carries after it.
    fn published(case: &str, value: &str) -> Self {
        let (authority, prefix) = match value.find('/') {
            Some(at) => (&value[..at], &value[at..]),
            None => (value, ""),
        };
        Self {
            case: case.to_string(),
            authority: authority.to_string(),
            prefix: prefix.to_string(),
        }
    }

    /// A host AWS itself names a domain's data plane by, addressed host-style: the whole path
    /// of the request stays the client's.
    fn host(case: &str, host: &str) -> Self {
        Self {
            case: case.to_string(),
            authority: host.to_string(),
            prefix: String::new(),
        }
    }

    /// The prefixed path-style address, which names the reserved namespace, this plane and the
    /// domain — by its own name or by a host it publishes — in the path, and so is resolved
    /// whatever host the request carries. Built from the constants that own those two segments
    /// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE` and this service's
    /// `state::ARN_NAMESPACE`) rather than spelled out here.
    fn prefixed(case: &str, named: &str) -> Self {
        Self {
            case: case.to_string(),
            authority: SERVICE_HOST.to_string(),
            prefix: format!(
                "/{}/{}/{named}",
                ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
                state::ARN_NAMESPACE
            ),
        }
    }
}

/// One signed data-plane request, dialled at `addressing`.
async fn answered(
    emulator: &Emulator,
    access_key_id: &str,
    addressing: &Addressing,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    dispatch_rest(
        emulator,
        access_key_id,
        &addressing.authority,
        method,
        &format!("{}{path}", addressing.prefix),
        body,
    )
    .await
}

/// What a client asks the cluster for, as `(case, method, path, body)`: the health poll it
/// opens with, and a search — a read of the documents the index holds.
fn routes() -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "the cluster health a client polls",
            "GET",
            "/_cluster/health".to_string(),
            None,
        ),
        (
            "a search",
            "POST",
            format!("/{INDEX}/_search"),
            Some(json!({})),
        ),
    ]
}

/// The cluster a domain's own data plane names itself by, so a served answer is read as THAT
/// domain's and not as any 200.
fn cluster_name(domain: &str) -> String {
    format!("{}:{domain}", ::engine::emulator_account_id())
}

/// RULE: every address a PUBLIC domain publishes reaches that domain's data plane, where the
/// cluster decides — served for the master user, refused `403 security_exception` for a
/// principal no role mapping binds, and served for that same principal once a mapping binds
/// it.
#[tokio::test]
async fn every_address_a_public_domain_publishes_serves_whoever_its_cluster_serves() {
    let emulator = Emulator::new();
    let name = "polled-public";
    let status = published(
        &emulator,
        name,
        &json!({ "IPAddressType": state::IP_ADDRESS_TYPE_DUALSTACK }),
        "Endpoint",
    )
    .await;
    let endpoint = status["Endpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a published Endpoint, got {status:?}"))
        .to_string();
    let dual_stack = status["EndpointV2"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a published EndpointV2, got {status:?}"))
        .to_string();
    let aws_host = endpoint_of(&emulator, name);

    let addressings = vec![
        Addressing::published(
            "the value DomainStatus$Endpoint publishes, dialled as published",
            &endpoint,
        ),
        Addressing::host("the public host AWS names that endpoint by", &aws_host),
        Addressing::host("the dual-stack host EndpointV2 reports", &dual_stack),
        Addressing::prefixed("the prefixed path naming the domain", name),
        Addressing::prefixed(
            "the prefixed path naming a host the domain publishes",
            &aws_host,
        ),
    ];

    // The session a client reaches the domain with, and the IAM grant of this service's
    // data-plane actions it carries, so what is left to decide is the cluster's part alone.
    given_session(&emulator);
    given_data_plane_policy(&emulator);

    // The MASTER user is granted everything, so the index the search rows read exists.
    let seeded = answered(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &addressings[0],
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({ "mappings": { "properties": { "title": { "type": "keyword" } } } })),
    )
    .await;
    assert_eq!(
        seeded.status, 200,
        "case \"the master user creating an index at the published address\": expected 200, \
         got {} with {:?}",
        seeded.status, seeded.body
    );

    for addressing in &addressings {
        for (route, method, path, body) in routes() {
            let case = format!("{route} at {}", addressing.case);
            let master = answered(
                &emulator,
                DEFAULT_ACCESS_KEY_ID,
                addressing,
                method,
                &path,
                body.clone(),
            )
            .await;
            assert!(
                (200..300).contains(&master.status),
                "case {case:?}: expected the master user to be served, got {} with {:?}",
                master.status,
                master.body
            );
            let refused =
                answered(&emulator, SESSION_KEY_ID, addressing, method, &path, body).await;
            assert_eq!(
                (refused.status, error_type(&refused)),
                (403, "security_exception".to_string()),
                "case {case:?} for a principal no mapping binds: expected the CLUSTER's own \
                 refusal, got {} with {:?}",
                refused.status,
                refused.body
            );
        }
        // And what the health answers is this domain's own cluster, read out of its record:
        // the address resolved the domain rather than any domain.
        let health = answered(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            addressing,
            "GET",
            "/_cluster/health",
            None,
        )
        .await;
        assert_eq!(
            health.body["cluster_name"].as_str(),
            Some(cluster_name(name).as_str()),
            "case {:?}: expected this domain's own cluster, got {:?}",
            addressing.case,
            health.body
        );
    }

    // Binding the role is the one change that makes the same requests serve, at every one of
    // those addresses.
    let mapped = answered(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &addressings[0],
        "PUT",
        "/_plugins/_security/api/rolesmapping/all_access",
        Some(json!({ "backend_roles": [delivery_role_arn()] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"the master user binding the role\": expected 201, got {} with {:?}",
        mapped.status, mapped.body
    );
    for addressing in &addressings {
        for (route, method, path, body) in routes() {
            let case = format!("{route} at {} once a mapping binds the role", addressing.case);
            let served =
                answered(&emulator, SESSION_KEY_ID, addressing, method, &path, body).await;
            assert!(
                (200..300).contains(&served.status),
                "case {case:?}: expected the mapped principal to be served, got {} with {:?}",
                served.status,
                served.body
            );
        }
    }
}

/// RULE, for the access mode that publishes its hosts in a map instead of a public endpoint:
/// each host `DomainStatus$Endpoints` reports reaches the domain's data plane too, where the
/// same cluster decides.
#[tokio::test]
async fn every_address_a_vpc_access_domain_publishes_reaches_its_data_plane() {
    let emulator = Emulator::new();
    let name = "polled-in-vpc";
    let status = published(
        &emulator,
        name,
        &json!({
            "IPAddressType": state::IP_ADDRESS_TYPE_DUALSTACK,
            "VPCOptions": {
                "SubnetIds": ["subnet-0123456789abcdef0"],
                "SecurityGroupIds": ["sg-0123456789abcdef0"],
            },
        }),
        "Endpoints",
    )
    .await;
    let host_of = |key: &str| {
        status["Endpoints"][key]
            .as_str()
            .unwrap_or_else(|| panic!("expected a published {key} host, got {status:?}"))
            .to_string()
    };
    let ipv4 = host_of(state::ENDPOINTS_KEY_VPC);
    let dual_stack = host_of(state::ENDPOINTS_KEY_VPC_DUALSTACK);
    // A VPC-access domain publishes no public endpoint at all, so the map is the whole of
    // what a client can be configured with.
    assert!(
        status["Endpoint"].is_null(),
        "case \"a VPC-access domain\": expected no public Endpoint, got {:?}",
        status["Endpoint"]
    );

    let addressings = vec![
        Addressing::host(
            "the IPv4 host Endpoints reports for the VPC endpoint",
            &ipv4,
        ),
        Addressing::host(
            "the dual-stack host Endpoints reports for the VPC endpoint",
            &dual_stack,
        ),
        Addressing::prefixed("the prefixed path naming the VPC-access host", &ipv4),
    ];

    given_session(&emulator);
    given_data_plane_policy(&emulator);

    for addressing in &addressings {
        let health = answered(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            addressing,
            "GET",
            "/_cluster/health",
            None,
        )
        .await;
        assert_eq!(
            (health.status, health.body["cluster_name"].as_str()),
            (200, Some(cluster_name(name).as_str())),
            "case \"the cluster health at {}\": expected this domain's own cluster, got {} \
             with {:?}",
            addressing.case,
            health.status,
            health.body
        );
        let refused = answered(
            &emulator,
            SESSION_KEY_ID,
            addressing,
            "GET",
            "/_cluster/health",
            None,
        )
        .await;
        assert_eq!(
            (refused.status, error_type(&refused)),
            (403, "security_exception".to_string()),
            "case \"a principal no mapping binds at {}\": expected the CLUSTER's own refusal, \
             got {} with {:?}",
            addressing.case,
            refused.status,
            refused.body
        );
    }
}

/// RULE, the other half: an address no domain of this store publishes is claimed by nothing,
/// so the front door answers `404 UnknownService` — the answer a client gets for an address
/// that names no data plane, and the one the published addresses above must never be answered
/// with.
///
/// The rows span the addresses that name no domain this store holds: the service's own API
/// host, a public endpoint host of the published SHAPE naming another domain, a VPC-access
/// host naming no endpoint of this store, the prefixed path naming a domain that does not
/// exist, and the authority a held domain publishes on another port — the port is part of
/// what identifies one domain's listener.
#[tokio::test]
async fn an_address_no_domain_publishes_is_claimed_by_nothing() {
    let emulator = Emulator::new();
    let name = "held";
    let status = published(&emulator, name, &json!({}), "Endpoint").await;
    let endpoint = status["Endpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a published Endpoint, got {status:?}"))
        .to_string();
    let aws_host = endpoint_of(&emulator, name);
    let held = Addressing::published("the address the held domain publishes", &endpoint);
    // The prefixed path-style addressing of a domain that does not exist, built the same way
    // as the one a held domain is reached by.
    let absent = Addressing::prefixed("a domain this store does not hold", "absent");

    // (the case, the host the request carries, the path it asks for)
    let mut rows: Vec<(String, String, String)> = vec![
        (
            "this service's own API host, which names no domain".to_string(),
            SERVICE_HOST.to_string(),
            "/_cluster/health".to_string(),
        ),
        (
            "a public endpoint host naming a domain this store does not hold".to_string(),
            aws_host.replacen(name, "absent", 1),
            "/_cluster/health".to_string(),
        ),
        (
            "a VPC-access host naming no endpoint of this store".to_string(),
            "vpc-endpoint-absent0123456789.us-east-1.es.amazonaws.com".to_string(),
            "/_cluster/health".to_string(),
        ),
        (
            "the prefixed path naming a domain this store does not hold".to_string(),
            absent.authority.clone(),
            format!("{}/_cluster/health", absent.prefix),
        ),
    ];
    // The published authority on ANOTHER port, for a domain published at a listener of its
    // own: another port of the same host is another plane, or this emulator's own endpoint.
    if let Some((host, port)) = held.authority.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            let other = if port == u16::MAX { port - 1 } else { port + 1 };
            rows.push((
                "the held domain's own host on another port".to_string(),
                format!("{host}:{other}"),
                "/_cluster/health".to_string(),
            ));
        }
    }

    for (case, host, path) in &rows {
        let answer = dispatch_rest(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            host,
            "GET",
            path,
            None,
        )
        .await;
        assert_eq!(
            (answer.status, error_code(&answer)),
            (404, "UnknownService".to_string()),
            "case {case:?}: expected the front door's refusal for an address nothing \
             publishes, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert!(
            answer.body["error"].is_null(),
            "case {case:?}: expected no cluster answer at all, got {:?}",
            answer.body
        );
    }

    // …while the address that domain DOES publish answers, so every row above is about the
    // address and not about the request.
    let served = answered(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &held,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        (served.status, served.body["cluster_name"].as_str()),
        (200, Some(cluster_name(name).as_str())),
        "case \"the address the held domain publishes\": expected its own cluster health, got \
         {} with {:?}",
        served.status,
        served.body
    );
}
