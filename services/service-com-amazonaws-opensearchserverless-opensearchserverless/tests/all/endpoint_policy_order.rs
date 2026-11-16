// SPDX-License-Identifier: CC0-1.0

//! In which ORDER the two policies a collection carries decide a request to its published
//! endpoint, end to end over the wire.
//!
//! Every case drives the real request path — `MultiServiceRouter::dispatch` with the method,
//! the path and the `Host` header a client of `CollectionDetail$collectionEndpoint` sends,
//! signed for the `aoss` signing name and carrying NO `x-amz-target`, because the OpenSearch
//! REST API a collection publishes has no RPC target.
//!
//! The rule under test, which is AWS's own order for the two policies: a `network` security
//! policy is what admits traffic to a collection's endpoints at all — "you must have at
//! least one network policy that applies to a collection in order to access its endpoint"
//! (AWS's "Network access for Amazon OpenSearch Serverless") — and only a request it admits
//! is then judged against the `data` access policies, which grant a PRINCIPAL permissions on
//! the collection and its indexes (AWS's "Data access control for Amazon OpenSearch
//! Serverless"). So a request to a collection no network policy covers is refused with the
//! endpoint's authentication failure whatever the data access policies would grant it, the
//! data-access denial is answered only where the endpoint is reachable, and in neither case
//! is the answer the control plane's RPC envelope: the published host serves the REST data
//! plane, not the `x-amz-target` dispatcher.
//!
//! The crate's two neighbouring suites each hold ONE of those decisions still: whether a
//! network policy names the collection (`collection_endpoint_authorization`, with the
//! account's own unbounded credential) and what a data access policy grants an IAM principal
//! at a reachable endpoint (the same suite). What is pinned HERE is the ORDER between them,
//! so the rows cross both policies with each other — a collection no network policy covers
//! whose data access policies DO grant the caller, and one whose do not — and with the
//! caller (an IAM principal the data access policies bound, and the account's own credential
//! they do not) and with the route asked for, which takes a different permission on a
//! different resource in each row.

use serde_json::{json, Value};

use crate::collection_endpoint_authorization::{
    active_collection, create_access_policy, create_security_policy, data_policy, emulator,
    endpoint_host, iam_user, knn_index_body, network_policy, rest, AMBIENT_ACCESS_KEY_ID,
};
use crate::collection_lifecycle::text;

/// The collection every case is made in. One router per case, so one name serves them all.
const COLLECTION: &str = "policy-order";

/// The index a case that READS or WRITES documents addresses, seeded by the account's own
/// credential wherever the endpoint is reachable at all.
const SEEDED_INDEX: &str = "seeded-index";

/// The error type a collection the network policies do not cover is refused under: the type
/// the endpoint's security layer answers an authentication failure with.
const SECURITY_EXCEPTION: &str = "security_exception";

/// The error type a principal no data access policy grants is refused under.
const FORBIDDEN: &str = "forbidden";

/// The envelope member this service's RPC front door words an error in (`__type`), which a
/// REST answer of the data plane never carries: a request to a published collection endpoint
/// that reached the `awsJson1_0` dispatcher was answered `{"__type":"SerializationException",
/// …}` for carrying no `x-amz-target`, so its absence is what says the data plane served the
/// request.
const RPC_ERROR_MEMBER: &str = "__type";

/// Who the data-plane request is signed by.
#[derive(Debug, Clone, Copy)]
enum Caller {
    /// An IAM user of this account, which the data access policies bound.
    Principal,
    /// The account's own published credential, which they do not.
    AccountOwn,
}

impl Caller {
    /// The label a created index of this caller's row is named with, so two callers of one
    /// router never ask to create the same index.
    fn label(self) -> &'static str {
        match self {
            Self::Principal => "principal",
            Self::AccountOwn => "account-own",
        }
    }

    /// Whether the data access policies decide anything about this caller.
    fn bounded_by_data_access(self) -> bool {
        matches!(self, Self::Principal)
    }
}

/// Which route of the REST surface the case asks for. Each takes a different permission on a
/// different resource, so the order under test is asserted over the whole surface rather than
/// over one verb.
#[derive(Debug, Clone, Copy)]
enum Route {
    /// `GET /` — the cluster banner, which takes `aoss:DescribeCollectionItems` on the
    /// COLLECTION resource.
    Banner,
    /// `PUT /{index}` — an index create, which takes `aoss:CreateIndex`.
    CreateIndex,
    /// `GET /{index}` — an index read, which takes `aoss:DescribeIndex`.
    DescribeIndex,
    /// `GET /{index}/_search` — a search, which takes `aoss:ReadDocument`.
    Search,
    /// `POST /{index}/_doc` — a document write, which takes `aoss:WriteDocument`.
    WriteDocument,
}

impl Route {
    /// The request this route sends, and the status the data plane SERVES it with.
    fn request(self, caller: Caller) -> (&'static str, String, Option<Value>, u16) {
        match self {
            Self::Banner => ("GET", "/".to_string(), None, 200),
            Self::CreateIndex => (
                "PUT",
                format!("/made-by-{}", caller.label()),
                Some(knn_index_body(4)),
                200,
            ),
            Self::DescribeIndex => ("GET", format!("/{SEEDED_INDEX}"), None, 200),
            Self::Search => (
                "GET",
                format!("/{SEEDED_INDEX}/_search"),
                Some(json!({ "query": { "match_all": {} } })),
                200,
            ),
            Self::WriteDocument => (
                "POST",
                format!("/{SEEDED_INDEX}/_doc"),
                Some(json!({ "vector": [1.0, 0.0, 0.0, 0.0] })),
                201,
            ),
        }
    }
}

/// RULE: the `network` security policies decide whether a collection's endpoint answers at
/// all, and only a request they admit is judged against the `data` access policies — so a
/// request to a collection no network policy covers is refused 401 whatever those policies
/// grant it, a 403 is answered only where the endpoint is reachable, and both refusals, like
/// every served answer, come from the REST data plane rather than from the control plane's
/// RPC envelope.
#[tokio::test]
async fn the_network_policies_decide_before_the_data_access_policies() {
    let routes = [
        Route::Banner,
        Route::CreateIndex,
        Route::DescribeIndex,
        Route::Search,
        Route::WriteDocument,
    ];
    let callers = [Caller::Principal, Caller::AccountOwn];
    // The two policy dimensions, crossed: whether a network policy covers the collection, and
    // whether the data access policies grant the signing principal everything or nobody but
    // someone else.
    for (network_case, admitted) in [
        ("no network policy at all", false),
        ("a network policy naming every collection", true),
    ] {
        for (data_case, grants_the_caller) in [
            ("a data access policy granting this principal", true),
            ("a data access policy granting another principal", false),
        ] {
            let router = emulator();
            if admitted {
                create_security_policy(
                    &router,
                    "network",
                    "net-policy",
                    &network_policy(&["collection/*"]),
                )
                .await;
            }
            let (caller_arn, caller_key) = iam_user(&router, "index-caller").await;
            let (other_arn, _) = iam_user(&router, "someone-else").await;
            let granted_to = if grants_the_caller {
                caller_arn.as_str()
            } else {
                other_arn.as_str()
            };
            // Both resource types a route of this surface addresses, so a row that is
            // refused is refused by the ORDER under test and not by a resource nobody
            // granted: the banner is a collection-level permission, every other route an
            // index-level one.
            let collection_resource = format!("collection/{COLLECTION}");
            let index_resource = format!("index/{COLLECTION}/*");
            create_access_policy(
                &router,
                "data-collection",
                &data_policy(
                    "collection",
                    &[collection_resource.as_str()],
                    &["aoss:*"],
                    &[granted_to],
                ),
            )
            .await;
            create_access_policy(
                &router,
                "data-index",
                &data_policy(
                    "index",
                    &[index_resource.as_str()],
                    &["aoss:*"],
                    &[granted_to],
                ),
            )
            .await;
            let collection = active_collection(&router, COLLECTION).await;
            let host = endpoint_host(&collection);

            // Wherever the endpoint is reachable, the index a read or a write addresses is
            // seeded by the account's own credential — which the data access policies do not
            // bound — so each row is about the decision under test and not about whether the
            // index is there.
            if admitted {
                let (status, answer) = rest(
                    &router,
                    &host,
                    AMBIENT_ACCESS_KEY_ID,
                    "PUT",
                    &format!("/{SEEDED_INDEX}"),
                    Some(&knn_index_body(4)),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {network_case} / {data_case}: setup expected the seed index create \
                     accepted, got {status} with {answer}"
                );
            }

            for route in routes {
                for caller in callers {
                    let case = format!("{network_case} / {data_case} / {route:?} / {caller:?}");
                    let (method, path, body, served) = route.request(caller);
                    let key = match caller {
                        Caller::Principal => caller_key.as_str(),
                        Caller::AccountOwn => AMBIENT_ACCESS_KEY_ID,
                    };
                    // What the rule says this row answers: the network decision first, the
                    // data-access decision only where the endpoint is reachable, and a
                    // served answer for a caller those policies admit or do not bound.
                    let (expected, expected_type) = if !admitted {
                        (401, Some(SECURITY_EXCEPTION))
                    } else if grants_the_caller || !caller.bounded_by_data_access() {
                        (served, None)
                    } else {
                        (403, Some(FORBIDDEN))
                    };
                    let (status, answer) =
                        rest(&router, &host, key, method, &path, body.as_ref()).await;
                    assert_eq!(
                        status, expected,
                        "case {case}: expected {method} {path} to answer {expected}, got \
                         {status} with {answer}"
                    );
                    assert!(
                        answer.get(RPC_ERROR_MEMBER).is_none(),
                        "case {case}: the published endpoint must be served by the REST data \
                         plane, not by the RPC front door, got {answer}"
                    );
                    if let Some(expected_type) = expected_type {
                        assert_eq!(
                            (
                                text(&answer, "error.type").as_str(),
                                answer["status"].as_u64()
                            ),
                            (expected_type, Some(u64::from(expected))),
                            "case {case}: expected the {expected} document of the data plane, \
                             got {answer}"
                        );
                        continue;
                    }
                    // A served route answers out of the collection's own state: the create
                    // reports the index it made, and the banner the collection it is served
                    // for.
                    match route {
                        Route::CreateIndex => assert_eq!(
                            text(&answer, "index"),
                            path.trim_start_matches('/'),
                            "case {case}: the create must report the index it made, got {answer}"
                        ),
                        Route::Banner => assert_eq!(
                            text(&answer, "cluster_name"),
                            COLLECTION,
                            "case {case}: the banner must name its own collection, got {answer}"
                        ),
                        _ => {}
                    }
                }
            }

            // Nothing a refused create asked for was performed: the account's own credential,
            // which the data access policies do not bound, finds no such index wherever it can
            // reach the endpoint at all.
            if admitted && !grants_the_caller {
                let refused = format!("/made-by-{}", Caller::Principal.label());
                let (held, body) =
                    rest(&router, &host, AMBIENT_ACCESS_KEY_ID, "HEAD", &refused, None).await;
                assert_eq!(
                    held, 404,
                    "case {network_case} / {data_case}: the refused create must have made no \
                     index, got {held} with {body}"
                );
            }
        }
    }
}
