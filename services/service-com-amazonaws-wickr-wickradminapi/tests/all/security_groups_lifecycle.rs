// SPDX-License-Identifier: CC0-1.0

//! The Wickr security-group lifecycle, driven over HTTP against one running
//! emulator.
//!
//! Every case here goes through the real multi-service router — route resolution,
//! restJson1 deserialization, modelled-constraint validation, the handler, and
//! response serialization — so what it asserts is the wire behaviour a client
//! sees, not an internal call.
//!
//! The rules under test, and where each comes from:
//!
//! * `CreateSecurityGroup` "creates a new security group in a Wickr network", and
//!   its response carries "the details of the newly created security group,
//!   including its ID, name, and settings", so every member of the group it
//!   answers with comes from what was stored and `GetSecurityGroup` reads the same
//!   record back.
//! * `SecurityGroupSettingsRequest` is "a subset of SecurityGroupSettings
//!   containing only the modifiable federation and security settings", each member
//!   naming the same member of `SecurityGroupSettings`, so a created group carries
//!   exactly the settings the request named.
//! * `federationMode` takes one of "0 (none), 1 (federated - all networks), 2
//!   (restricted - only permitted networks)", and
//!   `enableRestrictedGlobalFederation` "requires globalFederation to be enabled":
//!   neither shape carries a constraint trait, so the service refuses both with the
//!   modelled `ValidationError` naming the member.
//! * `clientToken` is "a unique identifier for this request to ensure idempotency"
//!   (`@idempotencyToken` on an `@idempotent` create), so a repeated create under
//!   one token reports the group the first call created rather than a second group.
//! * `SecurityGroup$isDefault` — "each network has only one default group" — and
//!   `activeMembers`/`botMembers` are "the number of active user members" and "the
//!   number of bot members currently in the security group", so both counts follow
//!   the users the store holds.
//! * A network, user, bot or security group "that doesn't exist or has been
//!   deleted" answers `ResourceNotFoundError` (HTTP 404).

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_wickr_wickradminapi::state::{
    DEFAULT_SECURITY_GROUP_ID, DEFAULT_SECURITY_GROUP_NAME, FEDERATION_MODES,
};

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; routing and the handlers are.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// The status `CreateNetwork` answers a create it carried out with (see
/// `src/ops/create_network.rs`).
const CREATE_NETWORK_STATUS: u16 = 201;

/// One request/response round trip: the HTTP status and the parsed JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_with_token(router, method, uri, body, None).await
}

/// The same round trip, carrying `client_token` in the `X-Client-Token` header the
/// model binds `CreateSecurityGroupRequest$clientToken` to.
async fn call_with_token(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
    client_token: Option<&str>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json");
    if let Some(token) = client_token {
        request = request.header("x-client-token", token);
    }
    let request = request
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

/// Create one security group, answering the status and the parsed body.
async fn create_group(
    router: &MultiServiceRouter,
    network_id: &str,
    name: &str,
    settings: Value,
    client_token: Option<&str>,
) -> (u16, Value) {
    call_with_token(
        router,
        "POST",
        &format!("/networks/{network_id}/security-groups"),
        Some(json!({ "name": name, "securityGroupSettings": settings })),
        client_token,
    )
    .await
}

/// Read one security group back.
async fn get_group(router: &MultiServiceRouter, network_id: &str, group_id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/networks/{network_id}/security-groups/{group_id}"),
        None,
    )
    .await
}

/// Create the users of one batch, each assigned to `group_id`.
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
        "BatchCreateUser into {group_id}: expected 200, got {status} with {body}"
    );
    assert!(
        body["failed"].as_array().is_none_or(|failed| failed.is_empty()),
        "BatchCreateUser into {group_id}: every item must be created, got {body}"
    );
}

/// The `__type` discriminator of an error body, or `""` when there is none.
fn error_code(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

/// The member a `ValidationError` names as the one that was refused.
fn refused_field(body: &Value) -> String {
    body["reasons"][0]["field"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The settings bodies a create is accepted with, one row per class of request:
/// no settings at all, each boolean, every mode `federationMode` names, and a
/// list member empty, of one entry and of several.
fn accepted_settings() -> Vec<(&'static str, Value)> {
    vec![
        ("no setting named", json!({})),
        (
            "every federation switch off",
            json!({
                "federationMode": FEDERATION_MODES[0],
                "globalFederation": false,
                "enableGuestFederation": false,
                "enableRestrictedGlobalFederation": false,
            }),
        ),
        (
            "federated locally, with a lockout threshold",
            json!({ "federationMode": FEDERATION_MODES[1], "lockoutThreshold": 5 }),
        ),
        (
            "restricted locally, with two permitted networks",
            json!({
                "federationMode": FEDERATION_MODES[2],
                "permittedNetworks": ["12345678", "87654321"],
            }),
        ),
        (
            "restricted locally, with no permitted network yet",
            json!({ "federationMode": FEDERATION_MODES[2], "permittedNetworks": [] }),
        ),
        (
            "restricted global federation over one AWS network",
            json!({
                "globalFederation": true,
                "enableRestrictedGlobalFederation": true,
                "permittedWickrAwsNetworks": [{ "region": "us-west-2", "networkId": "10000001" }],
            }),
        ),
        (
            "global federation over two enterprise networks",
            json!({
                "globalFederation": true,
                "permittedWickrEnterpriseNetworks": [
                    { "domain": "example.com", "networkId": "20000002" },
                    { "domain": "other.example", "networkId": "30000003" },
                ],
            }),
        ),
        (
            "guest federation on",
            json!({
                "enableGuestFederation": true,
                "globalFederation": true,
                "federationMode": FEDERATION_MODES[1],
            }),
        ),
        (
            "a lockout threshold of zero",
            json!({ "lockoutThreshold": 0 }),
        ),
    ]
}

#[tokio::test]
async fn a_created_security_group_carries_the_settings_the_request_named() {
    let router = emulator();
    let network_id = create_network(&router, "sg-net").await;
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for (case, settings) in accepted_settings() {
        let name = format!("group for {case}");
        let (status, created) =
            create_group(&router, &network_id, &name, settings.clone(), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {created}"
        );
        let group = created["securityGroup"].clone();

        let group_id = group["id"].as_str().unwrap_or_default().to_string();
        assert!(
            !group_id.is_empty() && group_id != DEFAULT_SECURITY_GROUP_ID,
            "case {case:?}: a created group needs an id of its own, got {group_id:?}"
        );
        assert!(
            minted.insert(group_id.clone()),
            "case {case:?}: id {group_id:?} was already handed to another group"
        );
        assert_eq!(
            group["name"],
            json!(name),
            "case {case:?}: expected the requested name, got {group}"
        );
        assert_eq!(
            group["isDefault"],
            json!(false),
            "case {case:?}: a created group is not the network's default one, got {group}"
        );
        assert_eq!(
            group["securityGroupSettings"], settings,
            "case {case:?}: expected exactly the requested settings, got {group}"
        );
        assert_eq!(
            group["activeMembers"],
            json!(0),
            "case {case:?}: a group nobody is in holds no active member, got {group}"
        );
        assert_eq!(
            group["botMembers"],
            json!(0),
            "case {case:?}: a group nobody is in holds no bot member, got {group}"
        );
        assert!(
            group["modified"].as_i64().is_some_and(|secs| secs > 0),
            "case {case:?}: modified must be the epoch second it was written, got {group}"
        );

        // READ BACK: the same record, through the operation that reports one group.
        let (status, fetched) = get_group(&router, &network_id, &group_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSecurityGroup expected 200, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["securityGroup"], group,
            "case {case:?}: GetSecurityGroup must report what the create stored, got {fetched}"
        );
    }

    // The group every network is created with is readable through the same operation.
    let (status, default_group) =
        get_group(&router, &network_id, DEFAULT_SECURITY_GROUP_ID).await;
    assert_eq!(
        status, 200,
        "the default group: expected 200, got {status} with {default_group}"
    );
    assert_eq!(
        default_group["securityGroup"]["isDefault"],
        json!(true),
        "the default group must report itself as the default one, got {default_group}"
    );
    assert_eq!(
        default_group["securityGroup"]["name"],
        json!(DEFAULT_SECURITY_GROUP_NAME),
        "the default group must keep the name it was seeded with, got {default_group}"
    );
}

#[tokio::test]
async fn a_security_group_setting_the_service_does_not_accept_is_refused() {
    let router = emulator();
    let network_id = create_network(&router, "sg-refuse").await;

    // `federationMode` names one of a closed set of modes, so each end of that set
    // and a value far outside it are refused; `enableRestrictedGlobalFederation`
    // narrows global federation, so it is refused unless the same request enables it.
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a mode below the first one",
            json!({ "federationMode": FEDERATION_MODES[0] - 1 }),
            "federationMode",
        ),
        (
            "a mode one past the last one",
            json!({ "federationMode": FEDERATION_MODES[FEDERATION_MODES.len() - 1] + 1 }),
            "federationMode",
        ),
        (
            "a mode far outside the set",
            json!({ "federationMode": 99 }),
            "federationMode",
        ),
        (
            "restricted global federation with nothing to restrict",
            json!({ "enableRestrictedGlobalFederation": true }),
            "enableRestrictedGlobalFederation",
        ),
        (
            "restricted global federation with global federation off",
            json!({
                "enableRestrictedGlobalFederation": true,
                "globalFederation": false,
                "permittedWickrAwsNetworks": [{ "region": "eu-west-1", "networkId": "40000004" }],
            }),
            "enableRestrictedGlobalFederation",
        ),
    ];

    for (case, settings, field) in rows {
        let (status, body) = create_group(&router, &network_id, "refused", settings, None).await;
        assert_eq!(
            status, 422,
            "case {case:?}: expected the modelled ValidationError (422), got {status} \
             with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {case:?}: expected ValidationError, got {body}"
        );
        assert_eq!(
            refused_field(&body),
            field,
            "case {case:?}: expected the refusal to name {field}, got {body}"
        );
        assert!(
            body["reasons"][0]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "case {case:?}: expected a reason beside the member, got {body}"
        );
    }

    // A refused create persisted nothing: `0` is the id of the group a network is
    // seeded with, so `1` is the first id a created group can carry and it is still
    // unused.
    let (status, body) = get_group(&router, &network_id, "1").await;
    assert_eq!(
        status, 404,
        "after the refusals: no group may have been stored, got {status} with {body}"
    );

    // And an acceptable create on the same network still works.
    let (status, created) = create_group(
        &router,
        &network_id,
        "accepted",
        json!({ "federationMode": FEDERATION_MODES[2], "permittedNetworks": ["12345678"] }),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "after the refusals: an acceptable create must succeed, got {status} with {created}"
    );
}

#[tokio::test]
async fn one_client_token_creates_one_security_group() {
    let router = emulator();
    let first_network = create_network(&router, "token-net-a").await;
    let second_network = create_network(&router, "token-net-b").await;
    let token = "create-sg-2026-01-02_x";
    let settings = json!({ "federationMode": FEDERATION_MODES[1] });

    let (status, first) = create_group(
        &router,
        &first_network,
        "token group",
        settings.clone(),
        Some(token),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"first create\": expected 200, got {status} with {first}"
    );
    let created = first["securityGroup"].clone();
    let group_id = created["id"].as_str().unwrap_or_default().to_string();

    // The SAME token, byte for byte the same request: the retry reports the group the
    // first call created.
    let (status, repeat) = create_group(
        &router,
        &first_network,
        "token group",
        settings.clone(),
        Some(token),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"retry\": expected 200, got {status} with {repeat}"
    );
    assert_eq!(
        repeat["securityGroup"], created,
        "case \"retry\": expected the group the first create made, got {repeat}"
    );

    // The same token with a different name is still that one create: nothing new is
    // minted and the stored group keeps the name it was created with.
    let (status, renamed) = create_group(
        &router,
        &first_network,
        "a different name",
        settings.clone(),
        Some(token),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"retry under another name\": expected 200, got {status} with {renamed}"
    );
    assert_eq!(
        renamed["securityGroup"], created,
        "case \"retry under another name\": expected the first group unchanged, got {renamed}"
    );

    // A different token is a different create.
    let (status, other) = create_group(
        &router,
        &first_network,
        "other group",
        settings.clone(),
        Some("create-sg-2026-01-02_y"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"another token\": expected 200, got {status} with {other}"
    );
    assert_ne!(
        other["securityGroup"]["id"], created["id"],
        "case \"another token\": expected a group of its own, got {other}"
    );

    // Two creates carrying no token at all are two creates.
    let mut untokened: BTreeSet<String> = BTreeSet::new();
    for attempt in 0..2 {
        let (status, body) = create_group(
            &router,
            &first_network,
            "untokened group",
            settings.clone(),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case \"no token\" attempt {attempt}: expected 200, got {status} with {body}"
        );
        let id = body["securityGroup"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            untokened.insert(id.clone()),
            "case \"no token\" attempt {attempt}: id {id:?} was handed out twice"
        );
    }

    // A token is scoped to the network it was used in, so the same one creates a group
    // in another network.
    let (status, elsewhere) = create_group(
        &router,
        &second_network,
        "token group",
        settings,
        Some(token),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the same token in another network\": expected 200, got {status} with \
         {elsewhere}"
    );
    let (status, fetched) = get_group(
        &router,
        &second_network,
        elsewhere["securityGroup"]["id"].as_str().unwrap_or_default(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the same token in another network\": the group must be readable there, got \
         {status} with {fetched}"
    );

    // And the group the token created in the first network is untouched by it.
    let (status, original) = get_group(&router, &first_network, &group_id).await;
    assert_eq!(
        status, 200,
        "case \"the first network's group\": expected 200, got {status} with {original}"
    );
    assert_eq!(
        original["securityGroup"], created,
        "case \"the first network's group\": a create in another network must not change \
         it, got {original}"
    );
}

#[tokio::test]
async fn security_group_member_counts_follow_the_stored_users() {
    let router = emulator();
    let network_id = create_network(&router, "members-net").await;
    let (status, created) = create_group(
        &router,
        &network_id,
        "engineering",
        json!({ "federationMode": FEDERATION_MODES[0] }),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the group: expected 200, got {status} with {created}"
    );
    let group_id = created["securityGroup"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // One row per count the store can be in: none, one, several, and several more
    // added to a group that already holds some.
    let rows: Vec<(&str, Vec<&str>, i64)> = vec![
        ("no member yet", vec![], 0),
        ("one member", vec!["a@example.com"], 1),
        ("three members", vec!["b@example.com", "c@example.com"], 3),
        ("one more member", vec!["d@example.com"], 4),
    ];
    for (case, usernames, expected) in rows {
        if !usernames.is_empty() {
            create_users(&router, &network_id, &group_id, &usernames).await;
        }
        let (status, fetched) = get_group(&router, &network_id, &group_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["securityGroup"]["activeMembers"],
            json!(expected),
            "case {case:?}: expected {expected} active members, got {fetched}"
        );
        assert_eq!(
            fetched["securityGroup"]["botMembers"],
            json!(0),
            "case {case:?}: nothing here creates a bot, so no group holds one, got {fetched}"
        );
    }

    // A user of one group is not a member of another: the default group still counts
    // only its own.
    create_users(
        &router,
        &network_id,
        DEFAULT_SECURITY_GROUP_ID,
        &["e@example.com", "f@example.com"],
    )
    .await;
    for (case, group, expected) in [
        ("the created group", group_id.as_str(), 4),
        ("the default group", DEFAULT_SECURITY_GROUP_ID, 2),
    ] {
        let (status, fetched) = get_group(&router, &network_id, group).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["securityGroup"]["activeMembers"],
            json!(expected),
            "case {case:?}: expected {expected} active members, got {fetched}"
        );
    }
}

#[tokio::test]
async fn a_security_group_whose_network_or_id_is_unknown_is_not_found() {
    let router = emulator();
    let network_id = create_network(&router, "missing-net").await;
    // Well formed per `NetworkId` (`^[0-9]{8}$`) and never minted by this store.
    let unknown_network = "99999999";

    let (status, body) = create_group(
        &router,
        unknown_network,
        "orphan",
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"create in an unknown network\": expected 404, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundError",
        "case \"create in an unknown network\": expected ResourceNotFoundError, got {body}"
    );

    for (case, network, group) in [
        ("read in an unknown network", unknown_network, "0"),
        ("an unknown group id", network_id.as_str(), "77"),
        ("an id that is not a number", network_id.as_str(), "engineering"),
    ] {
        let (status, body) = get_group(&router, network, group).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundError",
            "case {case:?}: expected ResourceNotFoundError, got {body}"
        );
    }

    // A network's groups go with the network: once it is deleted, neither reading nor
    // creating one finds anything.
    let (status, deleted) = call(&router, "DELETE", &format!("/networks/{network_id}"), None).await;
    assert_eq!(
        status, 200,
        "DeleteNetwork: expected 200, got {status} with {deleted}"
    );
    let (status, body) = get_group(&router, &network_id, DEFAULT_SECURITY_GROUP_ID).await;
    assert_eq!(
        status, 404,
        "case \"read after the network was deleted\": expected 404, got {status} with {body}"
    );
    let (status, body) = create_group(&router, &network_id, "late", json!({}), None).await;
    assert_eq!(
        status, 404,
        "case \"create after the network was deleted\": expected 404, got {status} with {body}"
    );
}
