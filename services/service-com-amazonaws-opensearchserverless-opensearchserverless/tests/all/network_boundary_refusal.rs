// SPDX-License-Identifier: CC0-1.0

//! What a collection's published endpoint ANSWERS a request its `network` security policies
//! do not admit, end to end over the wire.
//!
//! Every case drives the real request path — `MultiServiceRouter::dispatch` with the method,
//! the path and the `Host` header a client of `CollectionDetail$collectionEndpoint` sends,
//! signed for the `aoss` signing name and carrying no `x-amz-target`, because the OpenSearch
//! REST API a collection publishes has no RPC target.
//!
//! The rule under test: the network decision is taken at the endpoint's front door, before
//! the collection's own cluster is addressed at all, so a request no network policy admits is
//! answered with that front door's status and NO bytes — no layer that builds an error
//! document has run. Only a refusal the collection itself computed carries one: the data
//! access denial, which is the collection's authorization layer answering, carries its
//! OpenSearch error document. So the presence of a body is what tells the two refusal classes
//! apart, and a client can read which boundary refused it.
//!
//! The rows span what the rule generalises over rather than one recorded request: which route
//! is asked for (the collection itself and one index, read and written, with and without a
//! request body, under five methods), why no policy admits the request (none stored at all,
//! one naming another collection, one whose only rule is about another resource type), and —
//! for the second suite — the control-plane writer that takes the policy away and puts it
//! back while the collection stays live.
//!
//! The last two suites are about the other half of the same decision: the SOURCES a covering
//! statement states. A collection answers only a request from a source one of its covering
//! statements states, and a stated source refuses a request only on evidence that it comes
//! from somewhere else — so the first spans the source forms a statement has (public, named
//! endpoints, a named AWS service, an empty list, no source member at all) with endpoints
//! that place nobody on this emulator, and the second publishes a collection's address BEHIND
//! one stated endpoint and then rewrites that statement: narrowed to another endpoint of the
//! account the address stops answering, and widened back it answers again, out of the
//! document it held throughout. What the account's own DEFAULT network places instead is
//! pinned where both services stand up
//! (`core/router/tests/all/aoss_network_source_boundary.rs`).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::collection_endpoint_authorization::{
    active_collection, create_access_policy, create_security_policy, data_policy, emulator,
    endpoint_host, iam_user, knn_index_body, network_policy, rest, rest_bytes,
    AMBIENT_ACCESS_KEY_ID,
};
use crate::collection_lifecycle::{at, call, text};

/// The collection every case is made in. One router per case, so one name serves them all.
const COLLECTION: &str = "net-boundary";

/// The index the routes of a case address.
const INDEX: &str = "probe-index";

/// The document id a write and a read of a document address.
const DOCUMENT: &str = "doc-1";

/// The status the network front door refuses an inadmissible request with: a client of the
/// endpoint reads it as an authentication failure.
const NETWORK_REFUSAL: u16 = 401;

/// The error type the collection's own authorization layer refuses a principal its data
/// access policies grant nothing with — the one refusal class that DOES carry a document.
const FORBIDDEN: &str = "forbidden";

/// One route of the REST surface a case asks for: the method, the path and the body a client
/// sends, named by the case.
struct Route {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
}

/// Every route the cases span: the collection itself and one index, read and written, with
/// and without a request body, under five methods — each taking a different `aoss`
/// permission on a different resource, so no row is answered by the same arm of the surface
/// as another.
fn routes() -> Vec<Route> {
    vec![
        Route {
            case: "the cluster banner",
            method: "GET",
            path: "/".to_string(),
            body: None,
        },
        Route {
            case: "an index create",
            method: "PUT",
            path: format!("/{INDEX}"),
            body: Some(knn_index_body(4)),
        },
        Route {
            case: "an index read",
            method: "GET",
            path: format!("/{INDEX}"),
            body: None,
        },
        Route {
            case: "an index head",
            method: "HEAD",
            path: format!("/{INDEX}"),
            body: None,
        },
        Route {
            case: "an index delete",
            method: "DELETE",
            path: format!("/{INDEX}"),
            body: None,
        },
        Route {
            case: "a document write",
            method: "PUT",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: Some(json!({ "vector": [1.0, 0.0, 0.0, 0.0] })),
        },
        Route {
            case: "a document read",
            method: "GET",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: None,
        },
        Route {
            case: "a search",
            method: "POST",
            path: format!("/{INDEX}/_search"),
            body: Some(json!({ "query": { "match_all": {} } })),
        },
        Route {
            case: "a mapping read",
            method: "GET",
            path: format!("/{INDEX}/_mapping"),
            body: None,
        },
    ]
}

/// The name every suite below gives the collection's `network` security policy.
const NETWORK_POLICY: &str = "net-policy";

/// A VPC id of the form `CreateVpcEndpoint` takes (`VpcId`, pattern `^vpc-[0-9a-z]*$`).
const A_VPC: &str = "vpc-1a2b3c4d";

/// A second VPC id, so the endpoints a case names sit in two different networks.
const ANOTHER_VPC: &str = "vpc-9f8e7d6c";

/// A subnet id of the form `CreateVpcEndpoint` takes (`SubnetIds`, pattern
/// `^subnet-([0-9a-f]{8}|[0-9a-f]{17})$`).
const SUBNET: &str = "subnet-1a2b3c4d";

/// A well-formed `VpcEndpointId` the account holds NO endpoint for, which is what a policy
/// names when it was written against an endpoint of another account or a deleted one.
const UNHELD_ENDPOINT: &str = "vpce-0123456789abcdef0";

/// An AWS service a statement admits the traffic of, spelled as a `SourceServices` entry is.
const SOURCE_SERVICE: &str = "bedrock.amazonaws.com";

/// Create one managed interface endpoint in `vpc_id`, answering its minted `VpcEndpointId`.
async fn attach_vpc_endpoint(router: &MultiServiceRouter, name: &str, vpc_id: &str) -> String {
    let (status, code, created) = call(
        router,
        "CreateVpcEndpoint",
        json!({ "name": name, "vpcId": vpc_id, "subnetIds": [SUBNET] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateVpcEndpoint({name}): got {status} {code} with {created}"
    );
    let id = text(&created, "createVpcEndpointDetail.id");
    assert!(
        !id.is_empty(),
        "case CreateVpcEndpoint({name}): expected a minted id, got {created}"
    );
    id
}

/// A `network` policy STATEMENT covering [`COLLECTION`] that states `sources` — the members
/// a statement states its sources with, as the case wrote them.
fn statement_with_sources(sources: &Value) -> Value {
    let mut statement = json!({
        "Rules": [{ "ResourceType": "collection", "Resource": [format!("collection/{COLLECTION}")] }],
    });
    let (Some(statement_members), Some(sources)) =
        (statement.as_object_mut(), sources.as_object())
    else {
        panic!("a statement and its sources are both JSON objects");
    };
    for (member, value) in sources {
        statement_members.insert(member.clone(), value.clone());
    }
    statement
}

/// A `network` security policy document of ONE statement stating `sources`
/// ([`statement_with_sources`]), written as the list of statements such a document is.
fn policy_with_sources(sources: &Value) -> Value {
    json!([statement_with_sources(sources)])
}

/// REPLACE the collection's network policy with `policy`, through the control-plane writer
/// that replaces a policy document — which reads the current `policyVersion` first, the way
/// a client of `UpdateSecurityPolicy` does.
async fn replace_network_policy(router: &MultiServiceRouter, policy: &Value) {
    let (status, code, read) = call(
        router,
        "GetSecurityPolicy",
        json!({ "type": "network", "name": NETWORK_POLICY }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case GetSecurityPolicy(network/{NETWORK_POLICY}): got {status} {code} with {read}"
    );
    let version = text(&read, "securityPolicyDetail.policyVersion");
    let (status, code, updated) = call(
        router,
        "UpdateSecurityPolicy",
        json!({
            "type": "network",
            "name": NETWORK_POLICY,
            "policyVersion": version,
            "policy": policy.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case UpdateSecurityPolicy(network/{NETWORK_POLICY}): got {status} {code} with {updated}"
    );
}

/// A `network` security policy document whose only rule is about DASHBOARDS of this
/// collection — the other `ResourceType` a network policy's rules may name (AWS's "Network
/// access for Amazon OpenSearch Serverless"), which says nothing about the collection's own
/// endpoint.
fn dashboard_only_policy() -> Value {
    json!({
        "Rules": [{ "ResourceType": "dashboard", "Resource": [format!("collection/{COLLECTION}")] }],
        "AllowFromPublic": true,
    })
}

/// RULE: a request no `network` security policy admits is refused at the endpoint's front
/// door, with that front door's status and a body of zero bytes — whichever route it asks
/// for and whichever way no policy admits it — while a request a policy does admit is
/// answered by the collection itself.
#[tokio::test]
async fn a_network_refusal_carries_the_status_and_no_bytes_at_all() {
    /// One case: why no network policy admits the request, and the policy document stored
    /// (if any) before the collection is created.
    struct Unreachable {
        case: &'static str,
        policy: Option<Value>,
    }
    let unreachable = [
        Unreachable {
            case: "no network policy at all",
            policy: None,
        },
        Unreachable {
            case: "a network policy naming another collection",
            policy: Some(network_policy(&["collection/somewhere-else"])),
        },
        Unreachable {
            case: "a network policy whose only rule is about dashboards",
            policy: Some(dashboard_only_policy()),
        },
    ];
    for Unreachable { case, policy } in &unreachable {
        let router = emulator();
        if let Some(policy) = policy {
            create_security_policy(&router, "network", "net-policy", policy).await;
        }
        let collection = active_collection(&router, COLLECTION).await;
        let host = endpoint_host(&collection);
        for route in routes() {
            let (status, payload) = rest_bytes(
                &router,
                &host,
                AMBIENT_ACCESS_KEY_ID,
                route.method,
                &route.path,
                route.body.as_ref(),
            )
            .await;
            let served = String::from_utf8_lossy(&payload).to_string();
            assert_eq!(
                status, NETWORK_REFUSAL,
                "case {case} / {}: expected {} {} to answer {NETWORK_REFUSAL}, got {status} \
                 with {served:?}",
                route.case, route.method, route.path
            );
            assert!(
                payload.is_empty(),
                "case {case} / {}: a network refusal carries no bytes, got {} byte(s): \
                 {served:?}",
                route.case,
                payload.len()
            );
        }
    }

    // The control: under a policy that DOES name the collection, the very same routes are
    // answered by the collection itself — so the empty body above belongs to the refusal
    // class and not to the route that asked for it.
    let router = emulator();
    create_security_policy(
        &router,
        "network",
        "net-policy",
        &network_policy(&[format!("collection/{COLLECTION}").as_str()]),
    )
    .await;
    let collection = active_collection(&router, COLLECTION).await;
    let host = endpoint_host(&collection);
    for route in routes() {
        let (status, payload) = rest_bytes(
            &router,
            &host,
            AMBIENT_ACCESS_KEY_ID,
            route.method,
            &route.path,
            route.body.as_ref(),
        )
        .await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_ne!(
            status, NETWORK_REFUSAL,
            "case an admitted {}: the endpoint must answer rather than refuse, got {status} \
             with {served:?}",
            route.case
        );
        // A `HEAD` answers a status alone by definition; every other route of an admitted
        // request is answered with the document the collection computed.
        if route.method != "HEAD" {
            assert!(
                !payload.is_empty(),
                "case an admitted {}: expected the collection's own answer, got no bytes",
                route.case
            );
        }
    }
}

/// RULE: the endpoint's network boundary is the policy state the control plane holds at this
/// instant — taking the only policy that names a collection away stops its endpoint
/// answering (with the front door's empty refusal), putting it back makes the endpoint answer
/// again, and neither touches the collection's status, its published endpoint or the
/// documents it holds.
#[tokio::test]
async fn the_boundary_follows_what_the_control_plane_holds() {
    let policy_name = "net-policy";
    let policy = network_policy(&[format!("collection/{COLLECTION}").as_str()]);
    let router = emulator();
    create_security_policy(&router, "network", policy_name, &policy).await;
    let collection = active_collection(&router, COLLECTION).await;
    let collection_id = text(&collection, "id");
    let endpoint = text(&collection, "collectionEndpoint");
    let host = endpoint_host(&collection);

    // Live: the index is created and a document written and read back through the endpoint.
    let source = json!({ "vector": [1.0, 0.0, 0.0, 0.0], "label": "before-the-outage" });
    for (case, method, path, body) in [
        (
            "the index create",
            "PUT",
            format!("/{INDEX}"),
            Some(knn_index_body(4)),
        ),
        (
            "the document write",
            "PUT",
            format!("/{INDEX}/_doc/{DOCUMENT}"),
            Some(source.clone()),
        ),
    ] {
        let (status, answer) = rest(
            &router,
            &host,
            AMBIENT_ACCESS_KEY_ID,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        assert!(
            (200..300).contains(&status),
            "case {case}: expected the live endpoint to serve {method} {path}, got {status} \
             with {answer}"
        );
    }
    let read_path = format!("/{INDEX}/_doc/{DOCUMENT}");
    let (status, answer) = rest(
        &router,
        &host,
        AMBIENT_ACCESS_KEY_ID,
        "GET",
        &read_path,
        None,
    )
    .await;
    assert_eq!(
        (status, at(&answer, "_source").clone()),
        (200, source.clone()),
        "case the live document read: expected the stored source, got {status} with {answer}"
    );

    // The control plane takes the only policy that names the collection away.
    let (status, code, deleted) = call(
        &router,
        "DeleteSecurityPolicy",
        json!({ "type": "network", "name": policy_name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case DeleteSecurityPolicy(network/{policy_name}): got {status} {code} with {deleted}"
    );

    // The identical requests that were served a moment ago are now refused at the front
    // door, with no bytes for a client to parse.
    for (case, method, path, body) in [
        ("the document read", "GET", read_path.clone(), None),
        (
            "the document write",
            "PUT",
            read_path.clone(),
            Some(source.clone()),
        ),
    ] {
        let (status, payload) = rest_bytes(
            &router,
            &host,
            AMBIENT_ACCESS_KEY_ID,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_eq!(
            (status, payload.is_empty()),
            (NETWORK_REFUSAL, true),
            "case {case} with the policy deleted: expected {NETWORK_REFUSAL} and no bytes, \
             got {status} with {served:?}"
        );
    }

    // The collection itself is untouched by the refusal: same status, same published
    // endpoint.
    let (status, code, read) = call(&router, "BatchGetCollection", json!({ "ids": [collection_id] }))
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
        (200, "ACTIVE".to_string(), endpoint),
        "case the collection behind a refusing endpoint: got {status} {code} with {read}"
    );

    // Put the byte-identical policy back: the endpoint answers again, out of the documents
    // it held throughout.
    create_security_policy(&router, "network", policy_name, &policy).await;
    let (status, answer) = rest(
        &router,
        &host,
        AMBIENT_ACCESS_KEY_ID,
        "GET",
        &read_path,
        None,
    )
    .await;
    assert_eq!(
        (status, at(&answer, "_source").clone()),
        (200, source),
        "case the document read with the policy restored: expected the stored source \
         unchanged, got {status} with {answer}"
    );
}

/// RULE: only a refusal the collection itself computed carries an OpenSearch error document
/// — the data access denial, under the fixed type the endpoint's authorization layer answers
/// one with — while the network front door's refusal carries none, so the two classes are
/// distinguishable by what a client can read off them.
#[tokio::test]
async fn the_two_refusal_classes_are_told_apart_by_their_body() {
    /// One case: whether a network policy names the collection, and what the request is then
    /// refused with.
    struct Row {
        case: &'static str,
        admitted: bool,
        status: u16,
        /// The `error.type` of the document the refusal carries, or `None` for a refusal
        /// that carries no body at all.
        error_type: Option<&'static str>,
    }
    let rows = [
        Row {
            case: "no network policy admits the request",
            admitted: false,
            status: NETWORK_REFUSAL,
            error_type: None,
        },
        Row {
            case: "a network policy admits it and no data access policy grants it",
            admitted: true,
            status: 403,
            error_type: Some(FORBIDDEN),
        },
    ];
    for Row {
        case,
        admitted,
        status: expected,
        error_type,
    } in &rows
    {
        let router = emulator();
        if *admitted {
            create_security_policy(
                &router,
                "network",
                "net-policy",
                &network_policy(&[format!("collection/{COLLECTION}").as_str()]),
            )
            .await;
        }
        // A principal the data access policies DO bound, and a policy that grants everything
        // to somebody else: so where the endpoint is reachable at all, the refusal is the
        // collection's own authorization layer answering.
        let (_, caller_key) = iam_user(&router, "boundary-caller").await;
        let (other_arn, _) = iam_user(&router, "somebody-else").await;
        create_access_policy(
            &router,
            "data-index",
            &data_policy(
                "index",
                &[format!("index/{COLLECTION}/*").as_str()],
                &["aoss:*"],
                &[other_arn.as_str()],
            ),
        )
        .await;
        let collection = active_collection(&router, COLLECTION).await;
        let host = endpoint_host(&collection);
        for route in routes() {
            let (status, payload) = rest_bytes(
                &router,
                &host,
                &caller_key,
                route.method,
                &route.path,
                route.body.as_ref(),
            )
            .await;
            let served = String::from_utf8_lossy(&payload).to_string();
            assert_eq!(
                status, *expected,
                "case {case} / {}: expected {expected}, got {status} with {served:?}",
                route.case
            );
            let Some(error_type) = error_type else {
                assert!(
                    payload.is_empty(),
                    "case {case} / {}: the front door's refusal carries no bytes, got \
                     {served:?}",
                    route.case
                );
                continue;
            };
            // A `HEAD` answer carries no body whatever it says, so the DOCUMENT of the
            // authorization layer is read off the routes that have one; the status above is
            // what a `HEAD` caller reads the denial from.
            if route.method == "HEAD" {
                continue;
            }
            let document: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
            assert_eq!(
                (
                    text(&document, "error.type").as_str(),
                    at(&document, "status").as_u64()
                ),
                (*error_type, Some(u64::from(*expected))),
                "case {case} / {}: expected the authorization layer's own document, got \
                 {document}",
                route.case
            );
        }
    }
}

/// Assert that every route of the surface is answered by the collection itself rather than
/// refused at the endpoint's network front door.
async fn assert_every_route_is_answered(router: &MultiServiceRouter, case: &str, host: &str) {
    for route in routes() {
        let (status, payload) = rest_bytes(
            router,
            host,
            AMBIENT_ACCESS_KEY_ID,
            route.method,
            &route.path,
            route.body.as_ref(),
        )
        .await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_ne!(
            status, NETWORK_REFUSAL,
            "case {case} / {}: expected the collection to answer rather than refuse, got \
             {status} with {served:?}",
            route.case
        );
        // A `HEAD` answers a status alone by definition; every other admitted route is
        // answered with the document the collection computed.
        if route.method != "HEAD" {
            assert!(
                !payload.is_empty(),
                "case {case} / {}: expected the collection's own answer, got no bytes",
                route.case
            );
        }
    }
}

/// RULE: a collection answers only a request from a source one of its covering `network`
/// statements states — but a stated source refuses a request only on EVIDENCE that it comes
/// from somewhere else. Public access and an admitted AWS service state a source outright,
/// and a `SourceVPCEs` entry that places nobody states nothing about where a request comes
/// from: an id the account holds no endpoint for, an endpoint whose own placement nothing
/// answered about, an empty list, and a statement stating no source at all each refuse
/// nothing.
///
/// No EC2 is mounted on this emulator, so nothing answered about where ANY of the endpoints
/// created here are placed — which is what makes every row below the no-evidence case. What
/// the same statements refuse once EC2 has placed those endpoints is pinned in
/// `core/router/tests/all/aoss_network_source_boundary.rs`, where both services stand up.
///
/// The rows span the source forms a statement states and which endpoints it names; every row
/// rewrites the SAME policy through the control-plane writer, so each decision is read off
/// the document the service holds at that instant.
#[tokio::test]
async fn a_stated_source_that_places_nobody_refuses_nothing() {
    /// One case: the source members the statement states.
    struct Row {
        case: String,
        sources: Value,
    }
    let router = emulator();
    // Two endpoints of the account, in two different VPCs — neither placed by anything,
    // because this emulator has no EC2 to place them.
    let one = attach_vpc_endpoint(&router, "one-endpoint", A_VPC).await;
    let another = attach_vpc_endpoint(&router, "another-endpoint", ANOTHER_VPC).await;
    let rows: Vec<Row> = vec![
        Row {
            case: "public access, whatever endpoints it names beside it".to_string(),
            sources: json!({ "AllowFromPublic": true, "SourceVPCEs": [&another] }),
        },
        Row {
            case: "one endpoint of the account, placed by nothing".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&one] }),
        },
        Row {
            case: "two endpoints of the account in two VPCs, placed by nothing".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&one, &another] }),
        },
        Row {
            case: "an id the account holds no endpoint for".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [UNHELD_ENDPOINT] }),
        },
        Row {
            case: "an unheld id beside an endpoint of the account".to_string(),
            sources: json!({
                "AllowFromPublic": false,
                "SourceVPCEs": [&another, UNHELD_ENDPOINT],
            }),
        },
        Row {
            case: "an AWS service's own traffic beside an endpoint".to_string(),
            sources: json!({
                "AllowFromPublic": false,
                "SourceVPCEs": [&another],
                "SourceServices": [SOURCE_SERVICE],
            }),
        },
        Row {
            case: "an empty endpoint list".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [] }),
        },
        Row {
            case: "no source member at all".to_string(),
            sources: json!({}),
        },
        Row {
            case: "two statements, each stating one endpoint".to_string(),
            sources: Value::Null,
        },
    ];

    // The collection is made live under a policy that covers it, which every row then
    // replaces: the same collection, the same published address, one decision per document.
    create_security_policy(
        &router,
        "network",
        NETWORK_POLICY,
        &network_policy(&[format!("collection/{COLLECTION}").as_str()]),
    )
    .await;
    let collection = active_collection(&router, COLLECTION).await;
    let host = endpoint_host(&collection);
    for Row { case, sources } in &rows {
        // A document is a LIST of statements, and a collection two of them cover is admitted
        // by either: the row that states no sources of its own is that document.
        let policy = match sources {
            Value::Null => json!([
                statement_with_sources(&json!({ "SourceVPCEs": [&one] })),
                statement_with_sources(&json!({ "SourceVPCEs": [&another] })),
            ]),
            sources => policy_with_sources(sources),
        };
        replace_network_policy(&router, &policy).await;
        assert_every_route_is_answered(&router, case, &host).await;
    }
}

/// RULE: a collection answers only a request from a source one of its covering `network`
/// statements states — and a request dialling the address that collection PUBLISHED comes
/// through the interface endpoints its own statements stated when that address was opened, so
/// a statement later narrowed to another endpoint of the account states no source that
/// request is from and the endpoint stops answering, with the front door's empty refusal.
/// Widening it back makes the identical request answer again, out of the documents the
/// collection held throughout, and the collection stays ACTIVE under both.
///
/// The rows span which endpoints the narrowed statement names and in which source form —
/// the one the address was published behind, another endpoint of the account, both, an id the
/// account holds no endpoint for, public access, and an admitted AWS service.
#[tokio::test]
async fn an_endpoint_published_behind_one_source_stops_answering_when_it_is_narrowed_away() {
    /// One case: the source members the statement is rewritten with, and whether the
    /// published address then answers.
    struct Row {
        case: String,
        sources: Value,
        admitted: bool,
    }
    let router = emulator();
    let behind = attach_vpc_endpoint(&router, "behind-endpoint", A_VPC).await;
    let another = attach_vpc_endpoint(&router, "another-endpoint", ANOTHER_VPC).await;
    let admitting = policy_with_sources(&json!({
        "AllowFromPublic": false,
        "SourceVPCEs": [&behind],
    }));
    // The statement stating that one endpoint is in force BEFORE the collection is created,
    // so the address it publishes is published behind that endpoint.
    create_security_policy(&router, "network", NETWORK_POLICY, &admitting).await;
    let collection = active_collection(&router, COLLECTION).await;
    let collection_id = text(&collection, "id");
    let endpoint = text(&collection, "collectionEndpoint");
    let host = endpoint_host(&collection);

    // Live: the index is created and a document written through the published address.
    let document = json!({ "vector": [1.0, 0.0, 0.0, 0.0], "label": "written-from-behind" });
    let read_path = format!("/{INDEX}/_doc/{DOCUMENT}");
    for (case, method, path, body) in [
        (
            "the index create",
            "PUT",
            format!("/{INDEX}"),
            Some(knn_index_body(4)),
        ),
        (
            "the document write",
            "PUT",
            read_path.clone(),
            Some(document.clone()),
        ),
    ] {
        let (status, answer) = rest(
            &router,
            &host,
            AMBIENT_ACCESS_KEY_ID,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        assert!(
            (200..300).contains(&status),
            "case {case} under the stated source: expected the endpoint to serve \
             {method} {path}, got {status} with {answer}"
        );
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "narrowed to another endpoint of the account".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&another] }),
            admitted: false,
        },
        Row {
            case: "that other endpoint beside the one it was published behind".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&another, &behind] }),
            admitted: true,
        },
        Row {
            case: "narrowed to another endpoint, with public access added".to_string(),
            sources: json!({ "AllowFromPublic": true, "SourceVPCEs": [&another] }),
            admitted: true,
        },
        Row {
            case: "narrowed to another endpoint, with an AWS service admitted".to_string(),
            sources: json!({
                "AllowFromPublic": false,
                "SourceVPCEs": [&another],
                "SourceServices": [SOURCE_SERVICE],
            }),
            admitted: true,
        },
        Row {
            case: "narrowed to an id the account holds no endpoint for".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [UNHELD_ENDPOINT] }),
            admitted: true,
        },
        Row {
            case: "widened back to the endpoint it was published behind".to_string(),
            sources: json!({ "AllowFromPublic": false, "SourceVPCEs": [&behind] }),
            admitted: true,
        },
    ];
    for Row {
        case,
        sources,
        admitted,
    } in &rows
    {
        replace_network_policy(&router, &policy_with_sources(sources)).await;
        if *admitted {
            // The identical read answers, out of the document written before any rewrite.
            let (status, answer) = rest(
                &router,
                &host,
                AMBIENT_ACCESS_KEY_ID,
                "GET",
                &read_path,
                None,
            )
            .await;
            assert_eq!(
                (status, at(&answer, "_source").clone()),
                (200, document.clone()),
                "case {case}: expected the stored source unchanged, got {status} with {answer}"
            );
            continue;
        }
        // Both the read and the write are refused at the front door, with no bytes to parse.
        for (probe, method, body) in [
            ("the document read", "GET", None),
            ("the document write", "PUT", Some(document.clone())),
        ] {
            let (status, payload) = rest_bytes(
                &router,
                &host,
                AMBIENT_ACCESS_KEY_ID,
                method,
                &read_path,
                body.as_ref(),
            )
            .await;
            let served = String::from_utf8_lossy(&payload).to_string();
            assert_eq!(
                (status, payload.is_empty()),
                (NETWORK_REFUSAL, true),
                "case {case} / {probe}: expected {NETWORK_REFUSAL} and no bytes, got \
                 {status} with {served:?}"
            );
        }
        // The collection behind the refusing endpoint is untouched: same status, same
        // published address.
        let (status, code, read) =
            call(&router, "BatchGetCollection", json!({ "ids": [&collection_id] })).await;
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
            "case {case}: the collection itself is untouched, got {status} {code} with {read}"
        );
    }
}
