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

use serde_json::{json, Value};

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
