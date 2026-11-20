// SPDX-License-Identifier: CC0-1.0

//! `ListSecurityGroups` driven over HTTP against one running emulator.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response
//! serialization — so what it asserts is the wire behaviour a client sees.
//!
//! The rules under test, and where each comes from:
//!
//! * `ListSecurityGroups` "retrieves a paginated list of security groups in a specified
//!   Wickr network", so a listing reports the groups stored under the network the
//!   `networkId` label names and nothing else.
//! * Every member of `SecurityGroup` the model marks `@required` is reported, and the
//!   membership counts are the ones the store holds at the time of the read:
//!   `activeMembers` is "the number of active user members currently in the security
//!   group", `botMembers` "the number of bot members".
//! * The listing is `@paginated(items=securityGroups)`, so the last page carries no
//!   `nextToken`; `maxResults` is "the maximum number of security groups to return in a
//!   single page" and a page size below one is refused, as is a `nextToken` the service
//!   never issued.
//! * `sortFields` is "the field to sort security groups by. Accepted values include 'id'
//!   and 'name'", and `sortDirection` is 'ASC' or 'DESC' with "default is 'DESC'".
//! * A network "that doesn't exist or has been deleted" answers `ResourceNotFoundError`
//!   (HTTP 404) — the rule every network-scoped operation of this API but `GetNetwork`
//!   follows — so a `networkId` behind which there is no network is never answered with
//!   an empty listing.
//! * A read reports: calling it twice in a row answers the same thing, and the counts
//!   move only when the stored users move.
//!
//! One group per network is as much as the store can hold today: the only operation that
//! writes a security group is the network create, which seeds the network's default
//! group, so the cases below span the paging, sorting and membership dimensions of the
//! rule rather than the group count.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{
    DEFAULT_SECURITY_GROUP_ID, DEFAULT_SECURITY_GROUP_NAME, NETWORK_GONE_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The status `CreateNetwork` answers a create it carried out with (see
/// `src/ops/create_network.rs`).
const CREATE_NETWORK_STATUS: u16 = 201;

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the listing is.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// One request/response round trip: the HTTP status and the parsed JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, parsed)
}

/// Create a network and return its minted id, failing the test if it was refused.
async fn create_network(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await;
    assert_eq!(
        status, CREATE_NETWORK_STATUS,
        "CreateNetwork {name:?}: expected {CREATE_NETWORK_STATUS}, got {status} with {body}"
    );
    body["networkId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNetwork {name:?}: no networkId in {body}"))
        .to_string()
}

/// Put `usernames` in `group_id` of `network_id`, failing the test if any was refused.
async fn create_users(
    router: &MultiServiceRouter,
    network_id: &str,
    group_id: &str,
    usernames: &[&str],
) {
    let users: Vec<Value> = usernames
        .iter()
        .map(|username| json!({ "username": username, "securityGroupIds": [group_id] }))
        .collect();
    let (status, body) = call(
        router,
        "POST",
        &format!("/networks/{network_id}/users"),
        Some(json!({ "users": users })),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchCreateUser {usernames:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["successful"].as_array().map(Vec::len).unwrap_or(0),
        usernames.len(),
        "BatchCreateUser {usernames:?}: expected every user created, got {body}"
    );
}

/// The `securityGroups` of a listing body, or an empty slice.
fn groups(body: &Value) -> Vec<Value> {
    body["securityGroups"].as_array().cloned().unwrap_or_default()
}

/// The `__type` discriminator of an error body, or `""` when there is none.
fn error_code(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

/// List one network's security groups, asserting the listing was served.
async fn list(router: &MultiServiceRouter, case: &str, network_id: &str, query: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/networks/{network_id}/security-groups{query}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    body
}

#[tokio::test]
async fn a_listing_reports_the_groups_stored_under_the_named_network() {
    let router = emulator();
    let listed = create_network(&router, "groups-net").await;
    let other = create_network(&router, "other-net").await;

    // Every `@required` member of `SecurityGroup`, on the group a network is created
    // with.
    let body = list(&router, "a new network", &listed, "").await;
    let reported = groups(&body);
    assert_eq!(
        reported.len(),
        1,
        "case \"a new network\": expected the network's default group, got {body}"
    );
    let group = &reported[0];
    for (member, expected) in [
        ("id", json!(DEFAULT_SECURITY_GROUP_ID)),
        ("name", json!(DEFAULT_SECURITY_GROUP_NAME)),
        ("isDefault", json!(true)),
        ("activeMembers", json!(0)),
        ("botMembers", json!(0)),
    ] {
        assert_eq!(
            group[member], expected,
            "case \"a new network\": {member} expected {expected}, got {}",
            group[member]
        );
    }
    assert!(
        group["securityGroupSettings"].is_object(),
        "case \"a new network\": securityGroupSettings is required, got {group}"
    );
    let modified = group["modified"].as_i64().unwrap_or_default();
    assert!(
        modified > 0,
        "case \"a new network\": modified is the epoch second the group was last \
         changed at, got {group}"
    );
    assert!(
        body["nextToken"].as_str().is_none(),
        "case \"a new network\": the last page carries no nextToken, got {body}"
    );

    // The counts come from the stored users of THIS network, so they move with the
    // users and not with another network's.
    create_users(&router, &listed, DEFAULT_SECURITY_GROUP_ID, &["a@x.test"]).await;
    let after_one = list(&router, "one member", &listed, "").await;
    assert_eq!(
        groups(&after_one)[0]["activeMembers"],
        json!(1),
        "case \"one member\": expected the one stored member, got {after_one}"
    );
    create_users(
        &router,
        &listed,
        DEFAULT_SECURITY_GROUP_ID,
        &["b@x.test", "c@x.test"],
    )
    .await;
    let after_three = list(&router, "three members", &listed, "").await;
    assert_eq!(
        groups(&after_three)[0]["activeMembers"],
        json!(3),
        "case \"three members\": expected every stored member counted, got {after_three}"
    );
    let untouched = list(&router, "another network's groups", &other, "").await;
    assert_eq!(
        groups(&untouched)[0]["activeMembers"],
        json!(0),
        "case \"another network's groups\": a network reports only its own members, \
         got {untouched}"
    );

    // Reporting only: a second read answers exactly what the first did.
    let again = list(&router, "read twice", &listed, "").await;
    assert_eq!(
        again, after_three,
        "case \"read twice\": a read must not change what a later caller sees, got \
         {again} after {after_three}"
    );
    assert_eq!(
        groups(&again)[0]["modified"],
        json!(modified),
        "case \"read twice\": modified must not move because the group was read, got \
         {again}"
    );
}

/// The paging and sorting arguments the listing accepts, each serving the groups the
/// network holds.
///
/// The rows differ along every dimension the arguments span — page size (absent, one,
/// several, the documented maximum), sort field (absent, each accepted name), sort
/// direction (absent, each value `SortDirection` declares) and their combinations — and
/// each asserts the whole stored set came back on a page that reaches the end.
#[tokio::test]
async fn every_accepted_paging_and_sorting_argument_serves_the_stored_groups() {
    let router = emulator();
    let network_id = create_network(&router, "paging-net").await;
    create_users(
        &router,
        &network_id,
        DEFAULT_SECURITY_GROUP_ID,
        &["a@x.test", "b@x.test"],
    )
    .await;

    for (case, query) in [
        ("no arguments", ""),
        ("one item a page", "?maxResults=1"),
        ("more than are stored", "?maxResults=5"),
        ("the documented maximum", "?maxResults=100"),
        ("by id", "?sortFields=id"),
        ("by name", "?sortFields=name"),
        ("ascending", "?sortDirection=ASC"),
        ("descending", "?sortDirection=DESC"),
        ("by name, ascending, one a page", "?sortFields=name&sortDirection=ASC&maxResults=1"),
        ("by id, descending", "?sortFields=id&sortDirection=DESC"),
    ] {
        let body = list(&router, case, &network_id, query).await;
        let reported = groups(&body);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected every stored group on the page, got {body}"
        );
        assert_eq!(
            reported[0]["id"],
            json!(DEFAULT_SECURITY_GROUP_ID),
            "case {case:?}: expected the stored group, got {body}"
        );
        assert_eq!(
            reported[0]["activeMembers"],
            json!(2),
            "case {case:?}: the members are the stored ones whatever the ordering, got {body}"
        );
        assert!(
            body["nextToken"].as_str().is_none(),
            "case {case:?}: a page that reaches the end carries no nextToken, got {body}"
        );
    }
}

/// A paging or sorting argument the listing cannot serve is refused, naming the member
/// it refused — never ignored, and never answered as an empty listing.
#[tokio::test]
async fn an_argument_the_listing_cannot_serve_is_refused_naming_the_member() {
    let router = emulator();
    let network_id = create_network(&router, "refusal-net").await;

    for (case, query, field) in [
        ("a page of none", "?maxResults=0", "maxResults"),
        ("a negative page", "?maxResults=-7", "maxResults"),
        ("a token from nowhere", "?nextToken=zzz", "nextToken"),
        ("a token that is not a number", "?nextToken=6162", "nextToken"),
        ("a member that is not sortable", "?sortFields=modified", "sortFields"),
        ("a member of another shape", "?sortFields=username", "sortFields"),
        ("no field at all", "?sortFields=", "sortFields"),
    ] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/security-groups{query}"),
            None,
        )
        .await;
        assert_eq!(
            status, 422,
            "case {case:?}: expected the modelled ValidationError status 422, got \
             {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {case:?}: expected ValidationError, got {body}"
        );
        assert_eq!(
            body["reasons"][0]["field"],
            json!(field),
            "case {case:?}: the refusal must name {field}, got {body}"
        );
    }

    // A value outside `SortDirection` is refused by the model's own constraints, before
    // any handler runs: this service names that refusal `ValidationError` too, and the
    // status of a boundary refusal stays the protocol's.
    let (status, body) = call(
        &router,
        "GET",
        &format!("/networks/{network_id}/security-groups?sortDirection=sideways"),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a direction outside SortDirection\": expected the boundary refusal \
         status 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationError",
        "case \"a direction outside SortDirection\": expected this service's \
         ValidationError code, got {body}"
    );
}

/// A `networkId` behind which there is no network has no groups to report, and is
/// answered as the resource that "doesn't exist or has been deleted" rather than as an
/// empty listing.
#[tokio::test]
async fn a_network_that_does_not_exist_is_not_an_empty_listing() {
    let router = emulator();
    let deleted = create_network(&router, "deleted-net").await;
    let (status, body) = call(&router, "DELETE", &format!("/networks/{deleted}"), None).await;
    assert_eq!(
        status, 200,
        "DeleteNetwork: expected 200, got {status} with {body}"
    );
    // A well-formed id this store never minted: `NetworkId` ends at 99999999 and ids are
    // minted from the low end of the 8-digit range upwards.
    let never_minted = "99999999";

    for (case, network_id) in [
        ("a network that was deleted", deleted.as_str()),
        ("an id this store never minted", never_minted),
    ] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/security-groups"),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundError",
            "case {case:?}: expected ResourceNotFoundError, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(NETWORK_GONE_MESSAGE),
            "case {case:?}: expected this API's fixed sentence for a network id behind \
             which there is no network, got {body}"
        );
        assert!(
            body["securityGroups"].is_null(),
            "case {case:?}: a refusal reports no groups, got {body}"
        );
    }

    // A label outside `NetworkId` (`@length` 8, `@pattern ^[0-9]{8}$`) is refused by the
    // model's own constraints before the handler runs.
    for case in ["123", "1234567890", "abcdefgh"] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/networks/{case}/security-groups"),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the boundary refusal status 400, got {status} \
             with {body}"
        );
        assert_ne!(
            status, 501,
            "case {case:?}: a malformed id is a refusal, not an unimplemented \
             operation, got {status} with {body}"
        );
    }
}
