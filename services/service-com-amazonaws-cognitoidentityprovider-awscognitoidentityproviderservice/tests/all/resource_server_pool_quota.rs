// SPDX-License-Identifier: CC0-1.0

//! The per-user-pool resource-server ceiling, over the wire, driven through the
//! router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a user pool holds at most the quota's worth of resource
//! servers, so `CreateResourceServer` on a pool already holding that many is
//! refused `LimitExceededException` — "This exception is thrown when a user
//! exceeds the limit for a requested Amazon Web Services resource"
//! (`smithy_query(query="operation",operation="CreateResourceServer")`) — with a
//! 400 whose message states the limit the create was judged against; the refused
//! create stores NOTHING, and the ceiling is on the pool's STORED count rather
//! than on how many creates were attempted, so freeing a slot admits the
//! identical create again and another pool is bounded on its own count.
//!
//! The ceiling itself is DISCOVERED here rather than written down: each case
//! creates until the first refusal, so what is asserted is the rule, not one
//! number. Only the floor is cited, below. The rows differ along what the rule
//! generalises over — the form and length of the identifier, whether the create
//! carries custom scopes, how many slots are freed before refilling, and whether
//! one pool or two are being filled.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The fewest resource servers one pool must admit.
///
/// SOURCE: Service Quotas publishes the per-pool resource-server count for the
/// `cognito-idp` service code under quota code `L-7CDAF993` with this AWS default
/// value, and an account can ask to have it raised — so the enforced ceiling is
/// this number or more, never less. The cross-service suite
/// `core/router/tests/all/cognito_resource_server_quota_is_enforced.rs` pins the
/// enforced ceiling to the value that catalog serves; this suite pins the
/// enforcement behaviour itself inside the owning service.
const SMALLEST_ENFORCED_CEILING: usize = 25;

/// The most creates any case here will make before giving up, so a ceiling raised
/// past anything a fixture needs fails the suite instead of filling the store.
const MOST_CREATES_ATTEMPTED: usize = 400;

/// `ListResourceServers`'s modelled `MaxResults` maximum
/// (`ListResourceServersLimitType`, range 1..50), which is the biggest page this
/// suite can ask for.
const LARGEST_PAGE: usize = 50;

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Create a pool and return its minted `UserPoolId`.
async fn pool(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, name, "CreateUserPool", &json!({ "PoolName": name })).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: a pool carries a minted id, got {created}"))
        .to_string()
}

/// One row of the table: how a case spells the identifier of the resource server
/// at a given position, and whether its creates carry custom scopes.
struct Shape {
    case: &'static str,
    /// The identifier at `index`. Every form satisfies
    /// `ResourceServerIdentifierType`'s pattern `^[\x21\x23-\x5B\x5D-\x7E]+$` and
    /// its length 1..256.
    identifier: fn(usize) -> String,
    /// `true` sends a `Scopes` list with each create, `false` leaves it out.
    scopes: bool,
}

/// The table. The rows span the identifier forms the model admits — an API
/// friendly name, an API URL, punctuation the pattern admits, and the length
/// maximum 256 — and both a create that declares custom scopes and one that
/// declares none.
const SHAPES: &[Shape] = &[
    Shape {
        case: "an API friendly identifier, no Scopes member",
        identifier: friendly_identifier,
        scopes: false,
    },
    Shape {
        case: "an API URL identifier, with custom scopes",
        identifier: url_identifier,
        scopes: true,
    },
    Shape {
        case: "an identifier of punctuation the pattern admits",
        identifier: punctuated_identifier,
        scopes: true,
    },
    Shape {
        case: "an identifier at the modelled length maximum 256",
        identifier: longest_identifier,
        scopes: false,
    },
];

/// An API friendly name, the form the model's own example uses.
fn friendly_identifier(index: usize) -> String {
    format!("solar-system-data-{index:03}")
}

/// An API URL.
fn url_identifier(index: usize) -> String {
    format!("https://api-{index:03}.example.test/v2")
}

/// Punctuation the identifier pattern `^[\x21\x23-\x5B\x5D-\x7E]+$` admits: `!`
/// is `\x21`, `[` closes `\x23-\x5B` and `]` opens `\x5D-\x7E`, which `~` ends.
fn punctuated_identifier(index: usize) -> String {
    format!("tele!metry[{index:03}]~")
}

/// An identifier exactly 256 characters long, the modelled maximum.
fn longest_identifier(index: usize) -> String {
    let head = format!("{index:03}-");
    format!("{head}{}", "i".repeat(256 - head.len()))
}

impl Shape {
    /// This case's `CreateResourceServer` request for the resource server at
    /// `index`. The friendly name is spelled for `ResourceServerNameType`'s
    /// pattern `^[\w\s+=,.@-]+$`, which the identifier forms above do not all
    /// satisfy.
    fn request(&self, user_pool_id: &str, index: usize) -> Value {
        let mut body = json!({
            "UserPoolId": user_pool_id,
            "Identifier": (self.identifier)(index),
            "Name": format!("resource server {index:03}"),
        });
        if self.scopes {
            body["Scopes"] = json!([
                { "ScopeName": "read", "ScopeDescription": format!("read {index:03}") },
                { "ScopeName": "write", "ScopeDescription": format!("write {index:03}") },
            ]);
        }
        body
    }
}

/// Every `Identifier` the pool holds, walked a page at a time so a pool holding
/// more than one page is still reported in full.
async fn held_identifiers(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut body = json!({ "UserPoolId": user_pool_id, "MaxResults": LARGEST_PAGE });
        if let Some(token) = &next_token {
            body["NextToken"] = json!(token);
        }
        let page = ok(router, case, "ListResourceServers", &body).await;
        let entries = page["ResourceServers"].as_array().unwrap_or_else(|| {
            panic!("case {case:?}: a listing carries a ResourceServers list, got {page}")
        });
        for entry in entries {
            identifiers.push(
                entry["Identifier"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every entry names its identifier, got {entry}")
                    })
                    .to_string(),
            );
        }
        match page["NextToken"].as_str() {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
        assert!(
            identifiers.len() <= MOST_CREATES_ATTEMPTED,
            "case {case:?}: the listing must end, got {} entries so far",
            identifiers.len()
        );
    }
    identifiers
}

/// The outcome of filling a pool: how many creates were served before the first
/// refusal, and that refusal's status and body.
struct Filled {
    served: usize,
    status: u16,
    payload: Value,
}

/// Create resource servers in `user_pool_id` from `start` until one is refused.
async fn fill(
    router: &MultiServiceRouter,
    case: &str,
    shape: &Shape,
    user_pool_id: &str,
    start: usize,
) -> Filled {
    for index in start..start + MOST_CREATES_ATTEMPTED {
        let (status, payload) = call(
            router,
            "CreateResourceServer",
            &shape.request(user_pool_id, index),
        )
        .await;
        if status != 200 {
            return Filled {
                served: index - start,
                status,
                payload,
            };
        }
    }
    panic!(
        "case {case:?}: {MOST_CREATES_ATTEMPTED} creates on one pool were all served, so no \
         per-pool ceiling is enforced"
    );
}

/// Assert `filled`'s refusal is the one a create past the ceiling earns, and
/// return the ceiling its message states.
fn assert_refused_past_the_ceiling(case: &str, filled: &Filled, stored: usize) -> usize {
    assert_eq!(
        (filled.status, error_code(&filled.payload)),
        (400, "LimitExceededException"),
        "case {case:?}: a create past the ceiling is a 400 LimitExceededException, got {} with {}",
        filled.status,
        filled.payload
    );
    let message = error_message(&filled.payload);
    assert_eq!(
        message,
        format!("The limit for user pool resource servers is: {stored}."),
        "case {case:?}: the refusal must state the limit the create was judged against, got \
         {message:?}"
    );
    stored
}

#[tokio::test]
async fn a_create_past_the_pools_ceiling_is_refused_and_stores_nothing() {
    let router = router();

    for (row, shape) in SHAPES.iter().enumerate() {
        let case = shape.case;
        let user_pool_id = pool(&router, &format!("ceiling-pool-{row}")).await;

        let filled = fill(&router, case, shape, &user_pool_id, 0).await;
        assert!(
            filled.served >= SMALLEST_ENFORCED_CEILING,
            "case {case:?}: a pool must admit at least {SMALLEST_ENFORCED_CEILING} resource \
             servers, got {}",
            filled.served
        );
        let ceiling = assert_refused_past_the_ceiling(case, &filled, filled.served);

        // The refused create stored nothing: the identifier it named resolves to
        // no resource server, and the pool still holds exactly the served ones.
        let refused = (shape.identifier)(ceiling);
        let (status, described) = call(
            &router,
            "DescribeResourceServer",
            &json!({ "UserPoolId": user_pool_id, "Identifier": refused }),
        )
        .await;
        assert_eq!(
            (status, error_code(&described)),
            (400, "ResourceNotFoundException"),
            "case {case:?}: the refused identifier must resolve to no resource server, got \
             {described}"
        );
        let held = held_identifiers(&router, case, &user_pool_id).await;
        let expected: Vec<String> = (0..ceiling).map(shape.identifier).collect();
        assert_eq!(
            held.len(),
            ceiling,
            "case {case:?}: the pool must hold exactly the {ceiling} served resource servers, got \
             {} ({held:?})",
            held.len()
        );
        for identifier in &expected {
            assert!(
                held.contains(identifier),
                "case {case:?}: every served resource server must still be listed, {identifier:?} \
                 is missing from {held:?}"
            );
        }
        assert!(
            !held.contains(&refused),
            "case {case:?}: the refused identifier must not be listed, got {held:?}"
        );

        // Repeating the refused create changes nothing: it is refused the same way
        // and the pool's count stands.
        let again = call(
            &router,
            "CreateResourceServer",
            &shape.request(&user_pool_id, ceiling),
        )
        .await;
        assert_eq!(
            (again.0, error_code(&again.1)),
            (400, "LimitExceededException"),
            "case {case:?}: a repeated create past the ceiling is refused the same way, got {}",
            again.1
        );
        assert_eq!(
            held_identifiers(&router, case, &user_pool_id).await.len(),
            ceiling,
            "case {case:?}: a refused create must leave the pool's count alone"
        );
    }
}

#[tokio::test]
async fn the_ceiling_is_on_the_stored_count_so_freeing_slots_admits_creates() {
    let router = router();
    let shape = &SHAPES[1];
    let case = shape.case;
    let user_pool_id = pool(&router, "freed-slots").await;

    let filled = fill(&router, case, shape, &user_pool_id, 0).await;
    assert!(
        filled.served >= SMALLEST_ENFORCED_CEILING,
        "case {case:?}: a pool must admit at least {SMALLEST_ENFORCED_CEILING} resource servers, \
         got {}",
        filled.served
    );
    let ceiling = assert_refused_past_the_ceiling(case, &filled, filled.served);

    // Free a growing number of slots and watch exactly that many creates be
    // served again before the ceiling bites — the rows differ along how many
    // slots were freed, and the last frees every slot the pool holds.
    let mut next = ceiling;
    let mut deleted_from = 0;
    for freed in [1_usize, 2, 5] {
        let label = format!("{case}, {freed} slot(s) freed");
        for offset in 0..freed {
            ok(
                &router,
                &label,
                "DeleteResourceServer",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Identifier": (shape.identifier)(deleted_from + offset),
                }),
            )
            .await;
        }
        deleted_from += freed;

        let refilled = fill(&router, &label, shape, &user_pool_id, next).await;
        assert_eq!(
            refilled.served, freed,
            "case {label:?}: exactly the freed slots must admit a create, got {}",
            refilled.served
        );
        assert_refused_past_the_ceiling(&label, &refilled, ceiling);
        next += freed;
        assert_eq!(
            held_identifiers(&router, &label, &user_pool_id).await.len(),
            ceiling,
            "case {label:?}: the pool must be back at its ceiling"
        );
    }

    // The identical create a refusal named is served once a slot is free, and the
    // resource server it stores round-trips through the single-resource read.
    let refused_identifier = (shape.identifier)(next);
    let (status, payload) = call(
        &router,
        "CreateResourceServer",
        &shape.request(&user_pool_id, next),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "LimitExceededException"),
        "case {case:?}: the pool is at its ceiling again, so this create is refused, got {payload}"
    );
    ok(
        &router,
        case,
        "DeleteResourceServer",
        &json!({ "UserPoolId": user_pool_id, "Identifier": (shape.identifier)(deleted_from) }),
    )
    .await;
    let created = ok(
        &router,
        case,
        "CreateResourceServer",
        &shape.request(&user_pool_id, next),
    )
    .await;
    assert_eq!(
        created["ResourceServer"]["Identifier"],
        json!(refused_identifier),
        "case {case:?}: the identical previously refused create must now be served, got {created}"
    );
    let read = ok(
        &router,
        case,
        "DescribeResourceServer",
        &json!({ "UserPoolId": user_pool_id, "Identifier": refused_identifier }),
    )
    .await;
    assert_eq!(
        read["ResourceServer"], created["ResourceServer"],
        "case {case:?}: the admitted create must round-trip through the stored resource server, \
         got {read}"
    );

    // Emptying the pool restores the whole allowance.
    for identifier in held_identifiers(&router, case, &user_pool_id).await {
        ok(
            &router,
            case,
            "DeleteResourceServer",
            &json!({ "UserPoolId": user_pool_id, "Identifier": identifier }),
        )
        .await;
    }
    assert!(
        held_identifiers(&router, case, &user_pool_id).await.is_empty(),
        "case {case:?}: the pool must be empty before it is refilled"
    );
    let refilled = fill(&router, case, shape, &user_pool_id, 1_000).await;
    assert_eq!(
        refilled.served, ceiling,
        "case {case:?}: an emptied pool must admit its whole allowance again, got {}",
        refilled.served
    );
    assert_refused_past_the_ceiling(case, &refilled, ceiling);
}

#[tokio::test]
async fn each_pool_is_bounded_on_its_own_stored_count() {
    let router = router();
    let shape = &SHAPES[0];
    let case = shape.case;
    let first = pool(&router, "first-ceiling-pool").await;
    let second = pool(&router, "second-ceiling-pool").await;

    let filled = fill(&router, case, shape, &first, 0).await;
    let ceiling = assert_refused_past_the_ceiling(case, &filled, filled.served);

    // A pool that holds nothing admits the same allowance while the other one is
    // full, and the identifiers the full pool holds are free in it.
    for index in 0..ceiling {
        let (status, payload) = call(
            &router,
            "CreateResourceServer",
            &shape.request(&second, index),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second pool is bounded on its own count, so create {} must be \
             served, got {status} with {payload}",
            index + 1
        );
    }
    let (status, payload) = call(
        &router,
        "CreateResourceServer",
        &shape.request(&second, ceiling),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "LimitExceededException"),
        "case {case:?}: the second pool reaches the same ceiling on its own, got {payload}"
    );
    assert_eq!(
        held_identifiers(&router, case, &first).await.len(),
        ceiling,
        "case {case:?}: filling the second pool must not change the first pool's count"
    );

    // Freeing a slot in one pool does not free one in the other.
    ok(
        &router,
        case,
        "DeleteResourceServer",
        &json!({ "UserPoolId": second, "Identifier": (shape.identifier)(0) }),
    )
    .await;
    let (status, payload) = call(
        &router,
        "CreateResourceServer",
        &shape.request(&first, ceiling),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "LimitExceededException"),
        "case {case:?}: a slot freed in another pool must not admit a create here, got {payload}"
    );
}
