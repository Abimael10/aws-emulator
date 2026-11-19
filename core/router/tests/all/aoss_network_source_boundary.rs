// SPDX-License-Identifier: CC0-1.0

//! Where a request to a collection's published endpoint has to come FROM: the sources a
//! `network` security policy states, and what the endpoint answers a request from outside
//! them.
//!
//! RULE: a collection's endpoints answer only a request from a source one of the `network`
//! security policies covering that collection states — `AllowFromPublic` for anywhere, a
//! named AWS service's own traffic, or the managed interface endpoints `SourceVPCEs` names —
//! so narrowing a statement's sources to endpoints the request does not come through takes
//! that endpoint out of service, and widening them again puts it back, while the collection
//! itself and the documents it holds are untouched. A stated source refuses only on EVIDENCE
//! that the request comes from somewhere else: an id the account holds no endpoint for places
//! nobody, and so does an endpoint nothing resolved a network for.
//!
//! Two crates have to agree for that to hold, which is why this suite lives here rather than
//! in either of them: the collection's endpoint, its policies and its managed interface
//! endpoints are OpenSearch Serverless's, while a VPC — and whether it is the account's
//! DEFAULT network, which is the network a client that built none of its own is in — is
//! Amazon EC2's, resolved against the EC2 mounted in this very emulator when the interface
//! endpoint is created. What this suite is FOR is that EC2-resolved half: a statement naming
//! ANY endpoint the account placed in its default network states a source a request is from,
//! including one the collection's address was not published behind. The estate it stands up,
//! the `ec2Query` wire it asks EC2 over and the VPC it creates of its own are the sibling
//! suite's (`aoss_vpc_endpoint_placement_resolves_in_ec2.rs`): one builder and one writer for
//! this service pair, so neither suite can mount or create a different network than the
//! other.
//!
//! What the sibling suites pin, and this one does not repeat: the status and the empty body a
//! network refusal carries per route, that a stated source which places nobody refuses
//! nothing, and what the attachment a collection's address was PUBLISHED behind admits
//! (`services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
//! network_boundary_refusal.rs`), and the two refusal classes of the endpoint's authorization
//! layer (`collection_endpoint_authorization.rs` beside it).
//!
//! The rows span what the rule generalises over: which source members the statement states
//! (public, named endpoints, a named AWS service, an empty list, no member at all), which
//! endpoints it names (one in the account's default network, a second one in that same
//! network, one in a VPC of its own, an id the account holds no endpoint for, and lists
//! mixing them) and how many statements the document carries. Every wait is a poll for the
//! CONDITION under a generous deadline; nothing here asserts how long a step took.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::aoss_vpc_endpoint_placement_resolves_in_ec2::{ec2, ec2_vpc, emulator};
use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, aoss, at, encrypt_collections, rest, rest_bytes, text,
};
use crate::query_wire::element;

/// The collection every case of this suite is made in. One router per test, so one name
/// serves them all.
const COLLECTION: &str = "net-source-boundary";

/// The index the probes address.
const INDEX: &str = "probe-index";

/// The document id the write and the read of a document address.
const DOCUMENT: &str = "doc-1";

/// The status the endpoint's network front door refuses a request from an inadmissible source
/// with: a client of the endpoint reads it as an authentication failure.
const NETWORK_REFUSAL: u16 = 401;

/// A well-formed `VpcEndpointId` the account holds NO endpoint for, which is what a policy
/// names when it was written against an endpoint of another account or one since deleted.
const UNHELD_ENDPOINT: &str = "vpce-0123456789abcdef0";

/// An AWS service a statement admits the traffic of, spelled as a `SourceServices` entry is.
const SOURCE_SERVICE: &str = "bedrock.amazonaws.com";

/// The second octet of the CIDR block the VPC of this suite's own making carries, and the
/// Availability Zone its one subnet sits in — the shape [`ec2_vpc`] takes.
const OWN_VPC_OCTET: u8 = 92;
const OWN_VPC_ZONE: &str = "us-west-2a";

/// The status a created managed interface endpoint settles into, which is the state traffic
/// is routed through.
const VPC_ENDPOINT_ACTIVE: &str = "ACTIVE";

/// How many times a poll looks for a transition the clock owes, and how long one look waits
/// before the next: a generous deadline for a move the service owes in a few seconds. Every
/// wait here is for the CONDITION and nothing asserts how long it took.
const SETTLE_ATTEMPTS: usize = 4_800;
const SETTLE_PAUSE: Duration = Duration::from_millis(25);

/// One `Filter.N` parameter pair, as an `ec2Query` request spells a filter.
fn filter(name: &str, value: &str) -> [(String, String); 2] {
    [
        ("Filter.1.Name".to_string(), name.to_string()),
        ("Filter.1.Value.1".to_string(), value.to_string()),
    ]
}

/// The account's DEFAULT network in this Region and one of its subnets — the network a client
/// that created none of its own is in, discovered the way any client discovers it.
async fn default_network(router: &MultiServiceRouter) -> (String, String) {
    let by_default = filter("is-default", "true");
    let described = ec2(
        router,
        "DescribeVpcs",
        &[
            (by_default[0].0.as_str(), by_default[0].1.as_str()),
            (by_default[1].0.as_str(), by_default[1].1.as_str()),
        ],
    )
    .await;
    let vpc_id = element(&described, "vpcId");
    let by_vpc = filter("vpc-id", &vpc_id);
    let subnets = ec2(
        router,
        "DescribeSubnets",
        &[
            (by_vpc[0].0.as_str(), by_vpc[0].1.as_str()),
            (by_vpc[1].0.as_str(), by_vpc[1].1.as_str()),
        ],
    )
    .await;
    let subnet_id = element(&subnets, "subnetId");
    (vpc_id, subnet_id)
}

/// A VPC of the caller's OWN making and a subnet in it ([`ec2_vpc`], the one VPC-with-subnets
/// writer this service pair's suites share): a network that is not the account's default one.
async fn own_network(router: &MultiServiceRouter) -> (String, String) {
    let vpc = ec2_vpc(router, OWN_VPC_OCTET, &[OWN_VPC_ZONE]).await;
    let subnet = vpc
        .subnets
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("setup: expected a subnet in the VPC {}", vpc.id));
    (vpc.id, subnet)
}

/// Create one OpenSearch Serverless-managed interface endpoint in `(vpc_id, subnet_id)` and
/// poll until it reports `ACTIVE` — the state traffic is routed through — answering its minted
/// `VpcEndpointId`.
async fn interface_endpoint(
    router: &MultiServiceRouter,
    name: &str,
    vpc_id: &str,
    subnet_id: &str,
) -> String {
    let (status, created) = aoss(
        router,
        "CreateVpcEndpoint",
        json!({ "name": name, "vpcId": vpc_id, "subnetIds": [subnet_id] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateVpcEndpoint({name}) accepted, got {status} with {created}"
    );
    let id = text(&created, "createVpcEndpointDetail.id");
    assert!(
        !id.is_empty(),
        "setup: expected a minted VpcEndpointId, got {created}"
    );
    // The `PENDING -> ACTIVE` move is owed by the clock, so this looks again until the
    // endpoint reports `ACTIVE` or a generous deadline passes.
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = aoss(router, "BatchGetVpcEndpoint", json!({ "ids": [&id] })).await;
        assert_eq!(
            status, 200,
            "setup: expected BatchGetVpcEndpoint({id}) to answer, got {status} with {read}"
        );
        let detail = at(&read, "vpcEndpointDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == VPC_ENDPOINT_ACTIVE {
            return id;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("setup: the endpoint {name} never reached {VPC_ENDPOINT_ACTIVE}");
}

/// A `network` policy STATEMENT covering [`COLLECTION`] that states `sources` — the members a
/// statement states its sources with, as the case wrote them.
fn statement_with_sources(sources: &Value) -> Value {
    let mut statement = json!({
        "Rules": [{ "ResourceType": "collection", "Resource": [format!("collection/{COLLECTION}")] }],
    });
    let (Some(members), Some(sources)) = (statement.as_object_mut(), sources.as_object()) else {
        panic!("a statement and its sources are both JSON objects");
    };
    for (member, value) in sources {
        members.insert(member.clone(), value.clone());
    }
    statement
}

/// A `network` security policy document of ONE statement stating `sources`.
fn policy_with_sources(sources: &Value) -> Value {
    json!([statement_with_sources(sources)])
}

/// The name the suites give the collection's `network` security policy.
const NETWORK_POLICY: &str = "net-policy";

/// Store the collection's first `network` security policy.
async fn create_network_policy(router: &MultiServiceRouter, policy: &Value) {
    let (status, created) = aoss(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "network",
            "name": NETWORK_POLICY,
            "policy": policy.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the network policy accepted, got {status} with {created}"
    );
}

/// REPLACE that policy's document, through the control-plane writer that replaces one —
/// reading the current `policyVersion` first, the way a client of `UpdateSecurityPolicy`
/// does.
async fn replace_network_policy(router: &MultiServiceRouter, policy: &Value) {
    let (status, read) = aoss(
        router,
        "GetSecurityPolicy",
        json!({ "type": "network", "name": NETWORK_POLICY }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected GetSecurityPolicy(network/{NETWORK_POLICY}) to answer, got {status} \
         with {read}"
    );
    let (status, updated) = aoss(
        router,
        "UpdateSecurityPolicy",
        json!({
            "type": "network",
            "name": NETWORK_POLICY,
            "policyVersion": text(&read, "securityPolicyDetail.policyVersion"),
            "policy": policy.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected UpdateSecurityPolicy(network/{NETWORK_POLICY}) accepted, got \
         {status} with {updated}"
    );
}

/// The routes every case probes: the cluster itself, an index read under two methods, and a
/// document write and read — each a different `aoss` permission on a different resource, so
/// no row is answered by one arm of the surface alone.
fn probes() -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        ("the cluster banner", "GET", "/".to_string(), None),
        ("an index head", "HEAD", format!("/{INDEX}"), None),
        ("an index read", "GET", format!("/{INDEX}"), None),
        (
            "a document write",
            "PUT",
            format!("/{INDEX}/_doc/{DOCUMENT}"),
            Some(json!({ "label": "probe" })),
        ),
        (
            "a document read",
            "GET",
            format!("/{INDEX}/_doc/{DOCUMENT}"),
            None,
        ),
    ]
}

/// Assert what every probe is answered under the policy in force: refused at the network
/// front door with that status and no bytes, or answered by the collection itself.
async fn assert_admission(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    admitted: bool,
) {
    for (probe, method, path, body) in probes() {
        let (status, payload) = rest_bytes(router, endpoint, method, &path, body.as_ref()).await;
        let served = String::from_utf8_lossy(&payload).to_string();
        if admitted {
            assert_ne!(
                status, NETWORK_REFUSAL,
                "case {case} / {probe}: expected the collection to answer rather than \
                 refuse, got {status} with {served:?}"
            );
            continue;
        }
        assert_eq!(
            (status, payload.is_empty()),
            (NETWORK_REFUSAL, true),
            "case {case} / {probe}: expected {NETWORK_REFUSAL} and no bytes, got {status} \
             with {served:?}"
        );
    }
}

/// RULE: a collection answers only a request from a source one of its covering `network`
/// statements states — public access and an admitted AWS service state one outright, and
/// `SourceVPCEs` states the managed interface endpoints of the account it names, which admit
/// a request only where the account placed them in the network the request comes from (its
/// DEFAULT network, as EC2 reports it, for a client that built none of its own); an id the
/// account holds no endpoint for places nobody and so refuses nothing.
#[tokio::test]
async fn the_sources_a_statement_states_decide_which_requests_an_endpoint_answers() {
    /// One case: the source members the statement states, and whether the endpoint answers.
    struct Row {
        case: String,
        sources: Value,
        admitted: bool,
    }
    let (router, _engine) = emulator();
    let (default_vpc, default_subnet) = default_network(&router).await;
    let (own_vpc, own_subnet) = own_network(&router).await;
    // Two doors in the network the account's clients are in, and one in a network of its own
    // making — so a statement can name the right network, the wrong one, or neither.
    let inside = interface_endpoint(&router, "inside-endpoint", &default_vpc, &default_subnet).await;
    let beside = interface_endpoint(&router, "beside-endpoint", &default_vpc, &default_subnet).await;
    let outside = interface_endpoint(&router, "outside-endpoint", &own_vpc, &own_subnet).await;
    let rows: Vec<Row> = vec![
        Row {
            case: "public access, whatever endpoint it names beside it".to_string(),
            sources: json!({ "AllowFromPublic": true, "SourceVPCEs": [&outside] }),
            admitted: true,
        },
        Row {
            case: "an endpoint in the network the request comes from".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&inside] }),
            admitted: true,
        },
        Row {
            case: "a second endpoint in that same network".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&beside] }),
            admitted: true,
        },
        Row {
            case: "only an endpoint in a VPC of the caller's own making".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&outside] }),
            admitted: false,
        },
        Row {
            case: "an endpoint in the right network beside one outside it".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&outside, &inside] }),
            admitted: true,
        },
        Row {
            case: "an id the account holds no endpoint for".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [UNHELD_ENDPOINT] }),
            admitted: true,
        },
        Row {
            case: "an unheld id beside an endpoint outside the right network".to_string(),
            sources: json!({
                "AllowFromPublic": false,
                "SourceVPCEs": [&outside, UNHELD_ENDPOINT],
            }),
            admitted: true,
        },
        Row {
            case: "an AWS service's own traffic beside an endpoint outside it".to_string(),
            sources: json!({
                "AllowFromPublic": false,
                "SourceVPCEs": [&outside],
                "SourceServices": [SOURCE_SERVICE],
            }),
            admitted: true,
        },
        Row {
            case: "an empty endpoint list".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [] }),
            admitted: true,
        },
        Row {
            case: "no source member at all".to_string(),
            sources: json!({}),
            admitted: true,
        },
    ];

    encrypt_collections(&router).await;
    // The collection is made live under a statement that states the right network, which
    // every row then replaces: the same collection, the same published address, one decision
    // per document.
    create_network_policy(
        &router,
        &policy_with_sources(&json!({ "AllowFromPublic": false, "SourceVPCEs": [&inside] })),
    )
    .await;
    let collection = active_collection(&router, COLLECTION).await;
    let endpoint = text(&collection, "collectionEndpoint");
    for Row {
        case,
        sources,
        admitted,
    } in &rows
    {
        replace_network_policy(&router, &policy_with_sources(sources)).await;
        assert_admission(&router, case, &endpoint, *admitted).await;
    }

    // A document is a LIST of statements, and a collection two of them cover is admitted by
    // either: here the first states a network the request is not from and the second the one
    // it is.
    let two_statements = json!([
        statement_with_sources(&json!({ "AllowFromPublic": false, "SourceVPCEs": [&outside] })),
        statement_with_sources(&json!({ "AllowFromPublic": false, "SourceVPCEs": [&inside] })),
    ]);
    replace_network_policy(&router, &two_statements).await;
    assert_admission(
        &router,
        "one of two statements states the right network",
        &endpoint,
        true,
    )
    .await;
}

/// RULE: the source boundary is the policy document the control plane holds at this instant —
/// narrowing a statement's sources to an endpoint the request does not come through stops the
/// endpoint answering, with the front door's empty refusal; widening them back makes the
/// identical request answer again, out of the documents the collection held throughout, and
/// the collection stays ACTIVE under both.
#[tokio::test]
async fn narrowing_and_widening_the_sources_takes_the_endpoint_out_of_service_and_back() {
    let (router, _engine) = emulator();
    let (default_vpc, default_subnet) = default_network(&router).await;
    let (own_vpc, own_subnet) = own_network(&router).await;
    let inside = interface_endpoint(&router, "inside-endpoint", &default_vpc, &default_subnet).await;
    let outside = interface_endpoint(&router, "outside-endpoint", &own_vpc, &own_subnet).await;
    let admitting =
        policy_with_sources(&json!({ "AllowFromPublic": false, "SourceVPCEs": [&inside] }));

    encrypt_collections(&router).await;
    create_network_policy(&router, &admitting).await;
    let collection = active_collection(&router, COLLECTION).await;
    let collection_id = text(&collection, "id");
    let endpoint = text(&collection, "collectionEndpoint");

    // Live, from the network the statement states: the index is created and a document
    // written into it.
    let document = json!({ "label": "written-from-inside", "n": 1 });
    let read_path = format!("/{INDEX}/_doc/{DOCUMENT}");
    for (case, method, path, body) in [
        ("the index create", "PUT", format!("/{INDEX}"), None),
        (
            "the document write",
            "PUT",
            read_path.clone(),
            Some(document.clone()),
        ),
    ] {
        let (status, answer) = rest(&router, &endpoint, method, &path, body.as_ref()).await;
        assert!(
            (200..300).contains(&status),
            "case {case} from the stated network: expected the endpoint to serve \
             {method} {path}, got {status} with {answer}"
        );
    }
    let (status, answer) = rest(&router, &endpoint, "GET", &read_path, None).await;
    assert_eq!(
        (status, at(&answer, "_source")),
        (200, document.clone()),
        "case the document read from the stated network: expected the stored source, got \
         {status} with {answer}"
    );

    // The control plane narrows the statement to the endpoint in the caller's own VPC: the
    // identical requests are now refused at the front door, with no bytes to parse.
    replace_network_policy(
        &router,
        &policy_with_sources(&json!({ "AllowFromPublic": false, "SourceVPCEs": [&outside] })),
    )
    .await;
    for (case, method, body) in [
        ("the document read", "GET", None),
        ("the document write", "PUT", Some(document.clone())),
    ] {
        let (status, payload) =
            rest_bytes(&router, &endpoint, method, &read_path, body.as_ref()).await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_eq!(
            (status, payload.is_empty()),
            (NETWORK_REFUSAL, true),
            "case {case} with the sources narrowed: expected {NETWORK_REFUSAL} and no bytes, \
             got {status} with {served:?}"
        );
    }

    // The collection behind the refusing endpoint is untouched: same status, same address.
    let (status, read) = aoss(
        &router,
        "BatchGetCollection",
        json!({ "ids": [&collection_id] }),
    )
    .await;
    let detail = at(&read, "collectionDetails")
        .as_array()
        .and_then(|details| details.first())
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        (
            status,
            text(&detail, "status"),
            text(&detail, "collectionEndpoint")
        ),
        (200, "ACTIVE".to_string(), endpoint.clone()),
        "case the collection behind narrowed sources: got {status} with {read}"
    );

    // Widen the statement back to the network the request comes from: the identical read
    // answers out of the document the collection held throughout.
    replace_network_policy(&router, &admitting).await;
    let (status, answer) = rest(&router, &endpoint, "GET", &read_path, None).await;
    assert_eq!(
        (status, at(&answer, "_source")),
        (200, document),
        "case the document read with the sources widened again: expected the stored source \
         unchanged, got {status} with {answer}"
    );
}
