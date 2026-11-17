// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service answers a scope collection in, over the wire, driven exactly as
//! an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! Two rules, one per collection, because they are not the same kind of member:
//!
//! * An app client's `AllowedOAuthScopes` is a SET of scope text, which the pool holds
//!   ordered by the text itself: every read of the stored app client — and the update's
//!   own response, which renders what it stored — states that order, however the request
//!   sequenced the scopes, while the CREATE's response is the configuration it was handed
//!   and answers the submitted sequence. That is what the recorded service answers for an
//!   app client created with `["openid","email"]` and read back as `["email","openid"]`.
//! * A resource server's `Scopes` is an ordinary LIST of structures and keeps the sequence
//!   the request declared, through the create, the update, and every later read and
//!   listing — so a caller that indexes it positionally reads the same member back.
//!
//! The rows differ along what each rule generalises over: how many scopes the collection
//! holds (one, two, three, five), which permutation of the set the request wrote (every
//! one of them), how the names are spelled (standard OAuth scopes, a resource server's
//! custom scopes, punctuated names, a name at the modelled length maximum 256), and which
//! operation carried the collection (create, update, describe, list).
//!
//! No expectation here is taken from the code under test: the app-client order is a plain
//! sort of the row's own scope text, the resource-server order is the row's own submitted
//! sequence, and the one order the recordings carry is also written out as a literal.
//!
//! The harness (the engine, the router, the awsJson1_1 call) is the one
//! `log_delivery_configuration.rs` builds for this service, reached through the test crate
//! root rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::log_delivery_configuration::{ok, pool, router};

/// `scopes` in the order a pool holds the scopes an app client may authorize: the scope
/// text's own order, sorted here rather than asked of the service.
pub fn sorted_scopes_json<S: AsRef<str>>(scopes: &[S]) -> Value {
    let mut sorted: Vec<&str> = scopes.iter().map(AsRef::as_ref).collect();
    sorted.sort_unstable();
    json!(sorted)
}

/// `scopes` — `(ScopeName, ScopeDescription)` pairs — as the wire carries a resource
/// server's scope list, in the order they are given here.
fn scope_list_json<N: AsRef<str>, D: AsRef<str>>(scopes: &[(N, D)]) -> Value {
    Value::Array(
        scopes
            .iter()
            .map(|(scope_name, scope_description)| {
                json!({
                    "ScopeName": scope_name.as_ref(),
                    "ScopeDescription": scope_description.as_ref(),
                })
            })
            .collect(),
    )
}

/// Every permutation of `values`, so one row carries one SET in every sequence a caller
/// could have written it in. Bounded by the row's own length, which no row takes past
/// three.
fn permutations<T: Clone>(values: &[T]) -> Vec<Vec<T>> {
    if values.is_empty() {
        return vec![Vec::new()];
    }
    let mut permuted = Vec::new();
    for (at, value) in values.iter().enumerate() {
        let mut rest = values.to_vec();
        rest.remove(at);
        for mut tail in permutations(&rest) {
            tail.insert(0, value.clone());
            permuted.push(tail);
        }
    }
    permuted
}

/// The identifier of the resource server whose custom scopes the app-client rows reach
/// for, so a row can carry a custom scope beside the standard ones.
const RESOURCE_SERVER: &str = "https://orders.example.test";

/// One app-client row: a SET of scopes the pool authorizes, submitted in every sequence.
struct ClientScopeSet {
    case: &'static str,
    scopes: Vec<String>,
}

/// The app-client rows. The standard scopes are the ones every pool authorizes —
/// "`phone`, `email`, `openid`, and `profile`" beside `aws.cognito.signin.user.admin`
/// (`CreateUserPoolClient.AllowedOAuthScopes` in this service's model) — and the custom
/// ones are those [`RESOURCE_SERVER`] declares below.
fn client_scope_sets() -> Vec<ClientScopeSet> {
    vec![
        ClientScopeSet {
            case: "one standard scope",
            scopes: vec!["openid".to_string()],
        },
        ClientScopeSet {
            case: "the two standard scopes the recordings carry",
            scopes: vec!["openid".to_string(), "email".to_string()],
        },
        ClientScopeSet {
            case: "three standard scopes",
            scopes: vec![
                "phone".to_string(),
                "profile".to_string(),
                "aws.cognito.signin.user.admin".to_string(),
            ],
        },
        ClientScopeSet {
            case: "two custom scopes of a resource server beside a standard one",
            scopes: vec![
                format!("{RESOURCE_SERVER}/place"),
                "email".to_string(),
                format!("{RESOURCE_SERVER}/invoice.read"),
            ],
        },
    ]
}

/// One resource-server row: the custom scopes a declaration carries.
struct ServerScopeSet {
    case: &'static str,
    scopes: Vec<(String, String)>,
}

/// The resource-server rows: the three names the recordings carry, punctuated names the
/// member's pattern `^[\x21\x23-\x2E\x30-\x5B\x5D-\x7E]+$` admits, and a name at its
/// modelled length maximum 256 — the constraints
/// `smithy_query(query="shape",shape="ResourceServerScopeType")` publishes.
fn server_scope_sets() -> Vec<ServerScopeSet> {
    vec![
        ServerScopeSet {
            case: "one scope",
            scopes: vec![("read".to_string(), "Read access".to_string())],
        },
        ServerScopeSet {
            case: "the three scopes the recordings carry",
            scopes: vec![
                ("read".to_string(), "Read access".to_string()),
                ("write".to_string(), "Write access".to_string()),
                ("admin".to_string(), "Admin access".to_string()),
            ],
        },
        ServerScopeSet {
            case: "punctuated names and one at the length maximum",
            scopes: vec![
                ("tele!metry#1[2]~".to_string(), "Punctuated".to_string()),
                ("s".repeat(256), "At the length maximum".to_string()),
                ("a.b-c".to_string(), "Dotted".to_string()),
            ],
        },
    ]
}

/// The app client `client_id` names, as `DescribeUserPoolClient` renders it.
async fn described_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
) -> Value {
    ok(
        router,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await["UserPoolClient"]
        .clone()
}

/// The resource server `identifier` names, as `DescribeResourceServer` renders it.
async fn described_server(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    identifier: &str,
) -> Value {
    ok(
        router,
        "DescribeResourceServer",
        &json!({ "UserPoolId": user_pool_id, "Identifier": identifier }),
    )
    .await["ResourceServer"]
        .clone()
}

/// The resource server `identifier` names, as `ListResourceServers` serves it.
async fn listed_server(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    identifier: &str,
) -> Value {
    let listed = ok(
        router,
        "ListResourceServers",
        // The published `MaxResults` maximum is 50 and no test here stores more than a
        // handful of resource servers, so one page serves the whole listing.
        &json!({ "UserPoolId": user_pool_id, "MaxResults": 50 }),
    )
    .await;
    listed["ResourceServers"]
        .as_array()
        .unwrap_or_else(|| panic!("ResourceServers must be a present list, got {listed}"))
        .iter()
        .find(|entry| entry["Identifier"] == json!(identifier))
        .unwrap_or_else(|| panic!("the listing must serve {identifier:?}, got {listed}"))
        .clone()
}

/// Create the resource server whose custom scopes the app-client rows name.
async fn custom_scope_resource_server(router: &MultiServiceRouter, user_pool_id: &str) {
    ok(
        router,
        "CreateResourceServer",
        &json!({
            "UserPoolId": user_pool_id,
            "Identifier": RESOURCE_SERVER,
            "Name": "orders",
            "Scopes": [
                { "ScopeName": "place", "ScopeDescription": "Place an order" },
                { "ScopeName": "invoice.read", "ScopeDescription": "Read an invoice" },
            ],
        }),
    )
    .await;
}

#[tokio::test]
async fn an_app_clients_permitted_scopes_read_back_in_the_scope_texts_own_order() {
    let router = router();
    let user_pool_id = pool(&router, "client scopes", None).await;
    custom_scope_resource_server(&router, &user_pool_id).await;

    for set in client_scope_sets() {
        let case = set.case;
        // The order the pool holds the SET in: the scope text's own, sorted here rather
        // than asked of the service.
        let held = sorted_scopes_json(&set.scopes);
        for submitted in permutations(&set.scopes) {
            let label = format!("{case}, submitted as {submitted:?}");
            let created = ok(
                &router,
                "CreateUserPoolClient",
                &json!({
                    "UserPoolId": user_pool_id,
                    "ClientName": "scope-order-client",
                    "AllowedOAuthScopes": submitted,
                }),
            )
            .await["UserPoolClient"]
                .clone();

            // The create answers the configuration it was handed.
            assert_eq!(
                created["AllowedOAuthScopes"],
                json!(submitted),
                "case {label:?}: the create must answer the submitted sequence, got {created}"
            );

            // Every read of the stored app client states the held order, whatever
            // sequence configured it.
            let client_id = created["ClientId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {label:?}: a minted client id: {created}"))
                .to_string();
            let read = described_client(&router, &user_pool_id, &client_id).await;
            assert_eq!(
                read["AllowedOAuthScopes"], held,
                "case {label:?}: the read must state the held order {held}, got {read}"
            );
            let again = described_client(&router, &user_pool_id, &client_id).await;
            assert_eq!(
                again, read,
                "case {label:?}: a read must not change what the next read sees, got {again}"
            );

            // An update that replaces the set answers the held order itself, and the
            // reader agrees with it.
            let updated = ok(
                &router,
                "UpdateUserPoolClient",
                &json!({
                    "UserPoolId": user_pool_id,
                    "ClientId": client_id,
                    "AllowedOAuthScopes": submitted,
                }),
            )
            .await["UserPoolClient"]
                .clone();
            assert_eq!(
                updated["AllowedOAuthScopes"], held,
                "case {label:?}: the update must answer the held order {held}, got {updated}"
            );
            let after = described_client(&router, &user_pool_id, &client_id).await;
            assert_eq!(
                after["AllowedOAuthScopes"], held,
                "case {label:?}: the read after the update must state the held order, got {after}"
            );
        }
    }
}

#[tokio::test]
async fn a_resource_servers_scopes_keep_the_sequence_the_declaration_carried() {
    let router = router();
    let user_pool_id = pool(&router, "server scopes", None).await;

    for (row, set) in server_scope_sets().into_iter().enumerate() {
        let case = set.case;
        for (at, submitted) in permutations(&set.scopes).into_iter().enumerate() {
            let names: Vec<&str> = submitted.iter().map(|(name, _)| name.as_str()).collect();
            let label = format!("{case}, declared as {names:?}");
            // The expectation is the row's OWN submitted sequence.
            let declared = scope_list_json(&submitted);
            // One resource server per permutation, so each row's reads are its own.
            let identifier = format!("https://scope-order-{row}-{at}.example.test");
            let created = ok(
                &router,
                "CreateResourceServer",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Identifier": identifier,
                    "Name": "scope order",
                    "Scopes": declared,
                }),
            )
            .await["ResourceServer"]
                .clone();
            assert_eq!(
                created["Scopes"], declared,
                "case {label:?}: the create must answer the declared sequence, got {created}"
            );

            // Every rendering of the stored resource server keeps it.
            let read = described_server(&router, &user_pool_id, &identifier).await;
            assert_eq!(
                read["Scopes"], declared,
                "case {label:?}: the read must keep the declared sequence, got {read}"
            );
            let again = described_server(&router, &user_pool_id, &identifier).await;
            assert_eq!(
                again, read,
                "case {label:?}: a read must not change what the next read sees, got {again}"
            );
            assert_eq!(
                listed_server(&router, &user_pool_id, &identifier).await,
                read,
                "case {label:?}: the listing must serve what the read renders"
            );

            // And an update re-declaring the same scopes in the REVERSE sequence stores
            // that sequence: the member is a list, so the last declaration decides it.
            let mut reversed = submitted.clone();
            reversed.reverse();
            let redeclared = scope_list_json(&reversed);
            let updated = ok(
                &router,
                "UpdateResourceServer",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Identifier": identifier,
                    "Name": "scope order, updated",
                    "Scopes": redeclared,
                }),
            )
            .await["ResourceServer"]
                .clone();
            assert_eq!(
                updated["Scopes"], redeclared,
                "case {label:?}: the update must answer the sequence it declared, got {updated}"
            );
            assert_eq!(
                described_server(&router, &user_pool_id, &identifier).await,
                updated,
                "case {label:?}: the read after the update must render what it stored"
            );
            assert_eq!(
                listed_server(&router, &user_pool_id, &identifier).await,
                updated,
                "case {label:?}: the listing after the update must serve what it stored"
            );
        }
    }
}

#[tokio::test]
async fn the_recorded_app_client_scope_order_is_pinned_as_a_literal() {
    // The order the recorded service answers, written out here so the rule cannot pass by
    // agreeing with a broken derivation: the create echoes `["openid","email"]`, the read
    // of that same app client states `["email","openid"]`.
    let router = router();
    let user_pool_id = pool(&router, "pinned", None).await;

    let created = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "pinned-client",
            "AllowedOAuthScopes": ["openid", "email"],
        }),
    )
    .await["UserPoolClient"]
        .clone();
    assert_eq!(
        created["AllowedOAuthScopes"],
        json!(["openid", "email"]),
        "the create answers the submitted sequence, got {created}"
    );
    let client_id = created["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    let read = described_client(&router, &user_pool_id, &client_id).await;
    assert_eq!(
        read["AllowedOAuthScopes"],
        json!(["email", "openid"]),
        "the read states the held order, got {read}"
    );
}
